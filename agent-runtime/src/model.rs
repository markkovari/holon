//! Model clients. Deliberately plain text-in/text-out: tool use is a JSON
//! protocol the loop layers on top (see `agent.rs`), so the same agent runs on
//! Apple's on-device model, any OpenAI-compatible server, or Anthropic —
//! including small models with no native tool-calling.

use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

use crate::spec::ModelSpec;

#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub role: &'static str, // "user" | "assistant"
    pub content: String,
}

impl Msg {
    pub fn user(c: impl Into<String>) -> Self {
        Self { role: "user", content: c.into() }
    }
    pub fn assistant(c: impl Into<String>) -> Self {
        Self { role: "assistant", content: c.into() }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
}

pub struct Reply {
    pub text: String,
    pub usage: Usage,
}

/// Where `ModelSpec::Local` points — the runtime's own configuration, since
/// an agent's spec must not hard-code a port that changes every launch.
#[derive(Clone, Debug, Default)]
pub struct LocalModel {
    pub base_url: Option<String>,
    pub model: String,
}

pub fn complete(
    spec: &ModelSpec,
    local: &LocalModel,
    system: &str,
    messages: &[Msg],
    mock_cursor: &Mutex<usize>,
) -> Result<Reply, String> {
    match spec {
        ModelSpec::Local => {
            let base = local.base_url.as_deref().ok_or(
                "no local model is running (Apple `fm` is macOS-only); give this agent an \
                 OpenAi or Anthropic model instead",
            )?;
            let model = if local.model.is_empty() { "system" } else { &local.model };
            openai(base, model, "", system, messages)
        }
        ModelSpec::OpenAi { base_url, model, api_key_env } => {
            openai(base_url, model, &key(api_key_env)?, system, messages)
        }
        ModelSpec::Anthropic { model, api_key_env } => {
            anthropic(model, &key(api_key_env)?, system, messages)
        }
        ModelSpec::Mock { replies } => {
            let mut i = mock_cursor.lock().unwrap();
            let text = replies.get(*i).cloned().ok_or("mock model ran out of replies")?;
            *i += 1;
            let words = |s: &str| s.split_whitespace().count() as u64;
            let input = system.split_whitespace().count() as u64
                + messages.iter().map(|m| words(&m.content)).sum::<u64>();
            Ok(Reply { usage: Usage { input, output: words(&text) }, text })
        }
    }
}

fn key(env: &str) -> Result<String, String> {
    if env.is_empty() {
        return Ok(String::new());
    }
    std::env::var(env).map_err(|_| format!("environment variable {env} is not set"))
}

fn http() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())
}

fn openai(base: &str, model: &str, key: &str, system: &str, msgs: &[Msg]) -> Result<Reply, String> {
    let mut messages = vec![json!({"role": "system", "content": system})];
    messages.extend(msgs.iter().map(|m| json!({"role": m.role, "content": m.content})));
    let mut req = http()?
        .post(format!("{}/v1/chat/completions", base.trim_end_matches('/')))
        .json(&json!({"model": model, "messages": messages, "stream": false}));
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    let v = send(req)?;
    let text = v["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| format!("unexpected model response: {}", clip(&v.to_string())))?
        .to_string();
    Ok(Reply {
        text,
        usage: Usage {
            input: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            output: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        },
    })
}

fn anthropic(model: &str, key: &str, system: &str, msgs: &[Msg]) -> Result<Reply, String> {
    let messages: Vec<Value> =
        msgs.iter().map(|m| json!({"role": m.role, "content": m.content})).collect();
    let req = http()?
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({"model": model, "max_tokens": 2048, "system": system, "messages": messages}));
    let v = send(req)?;
    let text: String = v["content"]
        .as_array()
        .map(|a| a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""))
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("unexpected model response: {}", clip(&v.to_string())))?;
    Ok(Reply {
        text,
        usage: Usage {
            input: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
        },
    })
}

fn send(req: reqwest::blocking::RequestBuilder) -> Result<Value, String> {
    let r = req.send().map_err(|e| format!("model request failed: {e}"))?;
    let status = r.status();
    let body = r.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("model returned {status}: {}", clip(&body)));
    }
    serde_json::from_str(&body).map_err(|e| format!("model sent invalid JSON: {e}"))
}

fn clip(s: &str) -> String {
    s.chars().take(300).collect()
}
