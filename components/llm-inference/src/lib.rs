//! Deterministic mock for `llm:inference`: it imports nothing, so higher layers test offline.

#[allow(warnings)]
mod bindings;

use bindings::exports::llm::inference::inference::{
    Completion, Guest, InferError, Message, Options, Role, Usage,
};

struct Component;

const MODEL: &str = "mock-1";

const EMBED_DIMS: usize = 8;

fn est_tokens(chars: usize) -> u32 {
    (chars / 4) as u32
}

/// `messages` must be non-empty.
fn last_user_content(messages: &[Message]) -> &str {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::User))
        .or_else(|| messages.last())
        .map(|m| m.content.as_str())
        .unwrap_or("")
}

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

fn items_after(text: &str, marker: &str) -> Vec<String> {
    let Some(idx) = text.find(marker) else {
        return Vec::new();
    };
    let after = &text[idx + marker.len()..];
    // Stop at '.' too: callers' whitespace may be flattened ("fields: a, b. Reply with JSON" -> ["a","b"]).
    let end = after.find(['.', '\n']).unwrap_or(after.len());
    after[..end]
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect()
}

/// Only the escapes the mock can produce.
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
        let mut messages = Vec::with_capacity(2);
        if !system.is_empty() {
            messages.push(Message { role: Role::System, content: system });
        }
        messages.push(Message { role: Role::User, content: prompt });
        Self::chat(messages, opts)
    }

    fn embed(text: String, _opts: Options) -> Result<Vec<f32>, InferError> {
        let bytes = text.as_bytes();
        let mut sums = [0u64; EMBED_DIMS];
        for (i, &b) in bytes.iter().enumerate() {
            sums[i % EMBED_DIMS] += b as u64;
        }
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
