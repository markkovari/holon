//! `llm-inference` — the DETERMINISTIC MOCK provider for `llm:inference@0.1.0`.
//!
//! This is the reference implementation of the inference boundary: the SWAP
//! POINT that ai:assist (and, transitively, every app) imports. It does NOT
//! call a real model. There is no http, no wasi:config, no network, no clock,
//! no randomness here — the world `llm-inference` is intentionally minimal and
//! imports nothing. Given the same input this component always returns the same
//! output, which is exactly what makes the higher layers TESTABLE OFFLINE.
//!
//! A real provider (openai, anthropic, a local Ollama) is a SEPARATE component
//! that implements this same `inference` interface but additionally imports
//! `wasi:http` (to reach the vendor) and `wasi:config` (for base-url / model /
//! api-key). It is composed in at deployment time with `wac` in place of this
//! mock; ai:assist never names a vendor and cannot tell which one answered.
//! The mock exists so the wiring can be exercised without any of that.
//!
//! ## How the mock is USEFUL to ai:assist's tests
//!
//! ai:assist builds real prompts (classify / extract / summarize) on top of
//! `chat`. So the mock isn't a blind echo — it inspects the messages for a
//! directive keyword and produces a PARSEABLE, deterministic reply that those
//! higher-level helpers can assert on:
//!
//! * **Classify** — a message containing both `"Classify"` and `"labels:"`.
//!   The mock parses the labels from a line like `labels: a, b, c` and replies
//!   with EXACTLY the first label, `"a"`. So classification is deterministic.
//! * **Extract** — a message containing both `"Extract"` and `"fields:"`.
//!   The mock parses the field names from a line like `fields: name, email`
//!   and replies with a minimal JSON object mapping each field to
//!   `"mock-<field>"`, e.g. `{"name":"mock-name","email":"mock-email"}`. So
//!   extract gets valid JSON to parse.
//! * **Summarize** — a message containing `"Summarize"` or `"summary"`. The
//!   mock replies `"Summary: "` + the first 80 chars of the user content,
//!   collapsed to a single line.
//! * **Otherwise** — a plain echo: `"mock: "` + the user content.
//!
//! Directive detection scans the messages (system messages first, then the
//! rest) so test authors can drive behaviour purely by prompt content.
//!
//! Token usage is a rough deterministic estimate (chars / 4), never a real
//! tokenizer count. Embeddings are a stable 8-dim byte-bucket hash of the
//! text — same text in, same vector out, non-zero, but NOT semantically
//! meaningful. The only error this mock ever returns is `invalid-request` for
//! an empty message list; with no network there are no other failure paths.

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

// ---- helpers ------------------------------------------------------------

/// Rough, deterministic token estimate: ~4 chars per token.
fn est_tokens(chars: usize) -> u32 {
    (chars / 4) as u32
}

/// Content of the last `user` message, falling back to the last message of any
/// role if there is no user message. `messages` is assumed non-empty.
fn last_user_content(messages: &[Message]) -> &str {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::User))
        .or_else(|| messages.last())
        .map(|m| m.content.as_str())
        .unwrap_or("")
}

/// Concatenated text of every message, used for the prompt-token estimate and
/// for directive scanning.
fn all_content(messages: &[Message]) -> String {
    let mut s = String::new();
    for m in messages {
        s.push_str(&m.content);
    }
    s
}

/// Collapse any run of whitespace (incl. newlines) into single spaces and trim.
fn single_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Pull the comma-separated items that follow `marker:` (e.g. `labels:` or
/// `fields:`) anywhere in `text`. Returns the trimmed, non-empty items in order.
fn items_after(text: &str, marker: &str) -> Vec<String> {
    let Some(idx) = text.find(marker) else {
        return Vec::new();
    };
    let after = &text[idx + marker.len()..];
    // Stop the list at the first sentence boundary OR newline — whichever comes
    // first. (Callers' whitespace may be flattened, so we can't rely on a line
    // break alone; a trailing "." ends the field clause, e.g.
    // "fields: a, b. Reply with JSON" -> ["a","b"].)
    let end = after
        .find(['.', '\n'])
        .unwrap_or(after.len());
    after[..end]
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect()
}

/// Escape a string for embedding inside a JSON string literal. Only the bare
/// minimum the mock can produce (field names are simple words, but be safe).
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

/// Decide the reply text for a chat, given the full message set. Deterministic.
fn mock_reply(messages: &[Message]) -> String {
    let combined = all_content(messages);
    let user = last_user_content(messages);

    // Classify: reply with the first listed label.
    if combined.contains("Classify") && combined.contains("labels:") {
        let labels = items_after(&combined, "labels:");
        if let Some(first) = labels.first() {
            return first.clone();
        }
    }

    // Extract: reply with a JSON object mapping each field to "mock-<field>".
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

    // Summarize: "Summary: " + first 80 chars of user content, single line.
    if combined.contains("Summarize") || combined.contains("summary") {
        let line = single_line(user);
        let truncated: String = line.chars().take(80).collect();
        return format!("Summary: {truncated}");
    }

    // Otherwise: plain echo.
    format!("mock: {user}")
}

/// Build a `Completion` from the reply text and the input messages.
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

    fn complete(
        prompt: String,
        system: String,
        opts: Options,
    ) -> Result<Completion, InferError> {
        // Sugar over chat: an optional system message, then the user prompt.
        let mut messages = Vec::with_capacity(2);
        if !system.is_empty() {
            messages.push(Message {
                role: Role::System,
                content: system,
            });
        }
        messages.push(Message {
            role: Role::User,
            content: prompt,
        });
        Self::chat(messages, opts)
    }

    fn embed(text: String, _opts: Options) -> Result<Vec<f32>, InferError> {
        // Deterministic toy embedding: bucket the bytes into EMBED_DIMS slots by
        // (index % EMBED_DIMS), sum each bucket, then normalise to 0..1. Same
        // text always yields the same vector; non-zero for any non-empty text.
        // NOT a semantic embedding — just stable and shaped.
        let bytes = text.as_bytes();
        let mut sums = [0u64; EMBED_DIMS];
        for (i, &b) in bytes.iter().enumerate() {
            sums[i % EMBED_DIMS] += b as u64;
        }
        // Normalise by the max possible per-bucket sum so values land in 0..=1.
        // Each bucket holds ceil(len / DIMS) bytes, each at most 255.
        let per_bucket = (bytes.len() + EMBED_DIMS - 1) / EMBED_DIMS;
        let denom = (per_bucket as u64 * 255).max(1) as f32;
        let vec = sums.iter().map(|&s| s as f32 / denom).collect();
        Ok(vec)
    }

    fn describe() -> (String, bool) {
        // Default model + embeddings are available.
        (MODEL.to_string(), true)
    }
}

bindings::export!(Component with_types_in bindings);
