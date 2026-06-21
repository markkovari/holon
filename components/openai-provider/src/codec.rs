//! Pure OpenAI request-building + response-parsing, decoupled from the WIT
//! bindings so it is unit-testable on the host (`cargo test`). The Guest impl in
//! `lib.rs` converts the WIT records to/from these plain types at the edges.
//!
//! Everything here is deterministic and dependency-light (serde_json only) — no
//! WASI, no config, no HTTP. The HTTP plumbing lives in `lib.rs`; this is just
//! the codec, which is the part worth testing in isolation.

use serde::Deserialize;

/// A chat message (plain mirror of the WIT `message`).
pub struct Msg<'a> {
    pub role: &'a str, // "system" | "user" | "assistant"
    pub content: &'a str,
}

/// Completion tunables (plain mirror of the relevant WIT `options` fields).
#[derive(Default)]
pub struct Opts<'a> {
    pub model: &'a str, // already resolved (caller substitutes the default)
    pub temperature: u32,
    pub max_tokens: u32,
    pub stop: Vec<String>,
    pub seed: u64,
}

/// A parsed completion (plain mirror of the WIT `completion`).
pub struct Parsed {
    pub text: String,
    pub finish_reason: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// Why parsing a response failed (mapped to `infer-error` by the caller).
pub enum ParseError {
    BadResponse(String),
    NoContent,
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Build the `/v1/chat/completions` request body. `temperature` is milli-units
/// (700 -> 0.7). `model` must already be resolved (non-empty).
pub fn chat_body(messages: &[Msg], opts: &Opts) -> String {
    let mut parts = vec![format!("\"model\":{}", json_str(opts.model))];
    let msgs: Vec<String> = messages
        .iter()
        .map(|m| format!("{{\"role\":\"{}\",\"content\":{}}}", m.role, json_str(m.content)))
        .collect();
    parts.push(format!("\"messages\":[{}]", msgs.join(",")));
    if opts.temperature > 0 {
        parts.push(format!("\"temperature\":{}", opts.temperature as f64 / 1000.0));
    }
    if opts.max_tokens > 0 {
        parts.push(format!("\"max_tokens\":{}", opts.max_tokens));
    }
    if !opts.stop.is_empty() {
        let stops: Vec<String> = opts.stop.iter().map(|s| json_str(s)).collect();
        parts.push(format!("\"stop\":[{}]", stops.join(",")));
    }
    if opts.seed > 0 {
        parts.push(format!("\"seed\":{}", opts.seed));
    }
    format!("{{{}}}", parts.join(","))
}

/// Build the `/v1/embeddings` request body. `model` must already be resolved.
pub fn embed_body(text: &str, model: &str) -> String {
    format!("{{\"model\":{},\"input\":{}}}", json_str(model), json_str(text))
}

#[derive(Deserialize)]
struct ChatResp {
    #[serde(default)]
    choices: Vec<ChatChoice>,
    #[serde(default)]
    model: String,
    #[serde(default)]
    usage: Option<UsageResp>,
}
#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMsg,
    #[serde(default)]
    finish_reason: Option<String>,
}
#[derive(Deserialize)]
struct ChatMsg {
    #[serde(default)]
    content: String,
}
#[derive(Deserialize)]
struct UsageResp {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}
#[derive(Deserialize)]
struct EmbedResp {
    #[serde(default)]
    data: Vec<EmbedData>,
}
#[derive(Deserialize)]
struct EmbedData {
    #[serde(default)]
    embedding: Vec<f32>,
}

/// Parse a `/v1/chat/completions` response into the domain completion.
pub fn parse_completion(body: &[u8]) -> Result<Parsed, ParseError> {
    let parsed: ChatResp = serde_json::from_slice(body)
        .map_err(|e| ParseError::BadResponse(format!("chat json: {e}")))?;
    let choice = parsed.choices.into_iter().next().ok_or(ParseError::NoContent)?;
    if choice.message.content.is_empty() {
        return Err(ParseError::NoContent);
    }
    let usage = parsed.usage.unwrap_or(UsageResp { prompt_tokens: 0, completion_tokens: 0 });
    Ok(Parsed {
        text: choice.message.content,
        finish_reason: choice.finish_reason.unwrap_or_else(|| "other".to_string()),
        model: parsed.model,
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
    })
}

/// Parse a `/v1/embeddings` response into the first embedding vector.
pub fn parse_embedding(body: &[u8]) -> Result<Vec<f32>, ParseError> {
    let parsed: EmbedResp = serde_json::from_slice(body)
        .map_err(|e| ParseError::BadResponse(format!("embed json: {e}")))?;
    parsed
        .data
        .into_iter()
        .next()
        .map(|d| d.embedding)
        .filter(|v| !v.is_empty())
        .ok_or(ParseError::NoContent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_body_shapes_a_valid_openai_request() {
        let msgs = [
            Msg { role: "system", content: "You are a vet." },
            Msg { role: "user", content: "Summarize: Bella limps." },
        ];
        let opts = Opts { model: "gpt-4o-mini", temperature: 700, max_tokens: 256, ..Default::default() };
        let body = chat_body(&msgs, &opts);
        // valid JSON
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["model"], "gpt-4o-mini");
        assert_eq!(v["messages"][0]["role"], "system");
        assert_eq!(v["messages"][1]["role"], "user");
        assert_eq!(v["messages"][1]["content"], "Summarize: Bella limps.");
        // milli-units -> float
        assert_eq!(v["temperature"], 0.7);
        assert_eq!(v["max_tokens"], 256);
    }

    #[test]
    fn chat_body_omits_unset_optionals() {
        let msgs = [Msg { role: "user", content: "hi" }];
        let opts = Opts { model: "m", ..Default::default() };
        let v: serde_json::Value = serde_json::from_str(&chat_body(&msgs, &opts)).unwrap();
        assert!(v.get("temperature").is_none(), "temp 0 -> omitted (provider default)");
        assert!(v.get("max_tokens").is_none());
        assert!(v.get("seed").is_none());
    }

    #[test]
    fn chat_body_escapes_content() {
        let msgs = [Msg { role: "user", content: "quote \" and \n newline" }];
        let body = chat_body(&msgs, &Opts { model: "m", ..Default::default() });
        // must round-trip as valid JSON with the exact content preserved.
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["messages"][0]["content"], "quote \" and \n newline");
    }

    #[test]
    fn embed_body_shapes_a_valid_request() {
        let v: serde_json::Value =
            serde_json::from_str(&embed_body("golden retriever", "text-embedding-3-small")).unwrap();
        assert_eq!(v["model"], "text-embedding-3-small");
        assert_eq!(v["input"], "golden retriever");
    }

    #[test]
    fn parse_completion_reads_text_model_usage() {
        let body = br#"{"model":"gpt-4o-mini","choices":[{"message":{"role":"assistant","content":"Bella is limping."},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":7}}"#;
        let p = parse_completion(body).ok().unwrap();
        assert_eq!(p.text, "Bella is limping.");
        assert_eq!(p.finish_reason, "stop");
        assert_eq!(p.model, "gpt-4o-mini");
        assert_eq!(p.prompt_tokens, 11);
        assert_eq!(p.completion_tokens, 7);
    }

    #[test]
    fn parse_completion_empty_content_is_no_content() {
        let body = br#"{"choices":[{"message":{"content":""}}]}"#;
        assert!(matches!(parse_completion(body), Err(ParseError::NoContent)));
        let no_choices = br#"{"choices":[]}"#;
        assert!(matches!(parse_completion(no_choices), Err(ParseError::NoContent)));
    }

    #[test]
    fn parse_completion_bad_json_is_bad_response() {
        assert!(matches!(parse_completion(b"not json"), Err(ParseError::BadResponse(_))));
    }

    #[test]
    fn parse_embedding_reads_first_vector() {
        let body = br#"{"data":[{"embedding":[0.1,-0.2,0.3]}]}"#;
        let v = parse_embedding(body).ok().unwrap();
        assert_eq!(v, vec![0.1, -0.2, 0.3]);
    }

    #[test]
    fn parse_embedding_empty_is_no_content() {
        assert!(matches!(parse_embedding(br#"{"data":[]}"#), Err(ParseError::NoContent)));
    }
}
