//! `llm-inference` — the DETERMINISTIC MOCK provider for `llm:inference@0.1.0`.
//!
//! The reference implementation of the inference boundary: the SWAP POINT that ai:assist (and every app)
//! imports. It does NOT call a real model: no http, config, network, clock or randomness (the world imports
//! nothing), so the same input always gives the same output and higher layers are TESTABLE OFFLINE.
//!
//! A real provider (openai, anthropic, a local Ollama) is a SEPARATE component implementing the same
//! interface plus `wasi:http` and `wasi:config`, composed in with `wac` in place of this mock; ai:assist
//! cannot tell which answered.
//!
//! The mock is not a blind echo: it inspects messages for a directive keyword and gives a PARSEABLE,
//! deterministic reply that ai:assist's helpers (built on `chat`) can assert on:
//!
//! * **Classify** — a message with `"Classify"` and `"labels:"`: replies with EXACTLY the first label.
//! * **Extract** — a message with `"Extract"` and `"fields:"`: replies with a JSON object mapping each
//!   field to `"mock-<field>"`, e.g. `{"name":"mock-name","email":"mock-email"}`.
//! * **Summarize** — `"Summarize"` or `"summary"`: `"Summary: "` + the first 80 chars of the user content on one line.
//! * **Otherwise** — a plain echo: `"mock: "` + the user content.
//!
//! Directives are scanned system messages first, then the rest. Token usage is a rough estimate (chars / 4).
//! Embeddings are a stable 8-dim byte-bucket hash: same text, same vector, NOT semantic. The only error is
//! `invalid-request` for an empty message list.

#[allow(warnings)]
mod bindings;

use bindings::exports::llm::inference::inference::{
    Completion, Guest, InferError, Message, Options, Role, Usage,
};

struct Component;

/// The single model this mock pretends to be.
const MODEL: &str = "mock-1";

/// Dimensionality of the toy embedding.
const EMBED_DIMS: usize = 8;

fn est_tokens(chars: usize) -> u32 {
    (chars / 4) as u32
}

/// Content of the last `user` message, falling back to the last message of any role. `messages` is assumed non-empty.
fn last_user_content(messages: &[Message]) -> &str {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::User))
        .or_else(|| messages.last())
        .map(|m| m.content.as_str())
        .unwrap_or("")
}

/// Concatenated text of every message, for the prompt-token estimate and directive scanning.
fn all_content(messages: &[Message]) -> String {
    let mut s = String::new();
    for m in messages {
        s.push_str(&m.content);
    }
    s
}

fn single_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The comma-separated items after `marker:` (e.g. `labels:`, `fields:`) anywhere in `text`, trimmed and non-empty, in order.
fn items_after(text: &str, marker: &str) -> Vec<String> {
    let Some(idx) = text.find(marker) else {
        return Vec::new();
    };
    let after = &text[idx + marker.len()..];
    // Stop at the first sentence boundary OR newline: callers' whitespace may be flattened, so a trailing "."
    // ends the clause ("fields: a, b. Reply with JSON" -> ["a","b"]).
    let end = after.find(['.', '\n']).unwrap_or(after.len());
    after[..end]
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect()
}

/// Escape a string for a JSON string literal; only the bare minimum the mock can produce.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn mock_reply(messages: &[Message]) -> String {
    let combined = all_content(messages);
    let user = last_user_content(messages);

    if combined.contains("Classify") && combined.contains("labels:") {
        let labels = items_after(&combined, "labels:");
        if let Some(first) = labels.first() {
            return first.clone();
        }
    }

    if combined.contains("Extract") && combined.contains("fields:") {
        let fields = items_after(&combined, "fields:");
        if !fields.is_empty() {
            let body = fields
                .iter()
                .map(|f| {
                    let key = json_escape(f);
                    format!("\"{key}\":\"mock-{key}\"")
                })
                .collect::<Vec<_>>()
                .join(",");
            return format!("{{{body}}}");
        }
    }

    if combined.contains("Summarize") || combined.contains("summary") {
        let line = single_line(user);
        let truncated: String = line.chars().take(80).collect();
        return format!("Summary: {truncated}");
    }

    format!("mock: {user}")
}

fn completion_for(messages: &[Message]) -> Completion {
    let text = mock_reply(messages);
    let prompt_chars = all_content(messages).chars().count();
    Completion {
        finish_reason: "stop".to_string(),
        model: MODEL.to_string(),
        usage: Usage {
            prompt_tokens: est_tokens(prompt_chars),
            completion_tokens: est_tokens(text.chars().count()),
        },
        text,
    }
}

impl Guest for Component {
    fn chat(messages: Vec<Message>, _opts: Options) -> Result<Completion, InferError> {
        if messages.is_empty() {
            return Err(InferError::InvalidRequest("no messages".to_string()));
        }
        Ok(completion_for(&messages))
    }

    fn complete(prompt: String, system: String, opts: Options) -> Result<Completion, InferError> {
        // Sugar over chat: an optional system message, then the user prompt.
        let mut messages = Vec::with_capacity(2);
        if !system.is_empty() {
            messages.push(Message { role: Role::System, content: system });
        }
        messages.push(Message { role: Role::User, content: prompt });
        Self::chat(messages, opts)
    }

    fn embed(text: String, _opts: Options) -> Result<Vec<f32>, InferError> {
        // Deterministic toy embedding: bytes bucketed into EMBED_DIMS slots by (index % EMBED_DIMS), summed, then
        // normalised to 0..1. Stable and non-zero for non-empty text; NOT semantic.
        let bytes = text.as_bytes();
        let mut sums = [0u64; EMBED_DIMS];
        for (i, &b) in bytes.iter().enumerate() {
            sums[i % EMBED_DIMS] += b as u64;
        }
        // Normalise by the max possible per-bucket sum (ceil(len / DIMS) bytes, each at most 255).
        let per_bucket = bytes.len().div_ceil(EMBED_DIMS);
        let denom = (per_bucket as u64 * 255).max(1) as f32;
        let vec = sums.iter().map(|&s| s as f32 / denom).collect();
        Ok(vec)
    }

    fn describe() -> (String, bool) {
        (MODEL.to_string(), true)
    }
}

bindings::export!(Component with_types_in bindings);
