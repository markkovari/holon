//! Pure, host-testable side of the provider; the chat wire shape is holon-llm's (ADR-0116), embeddings stay here.

use holon_llm::{openai, Message, Part, Request, Role};
use serde::Deserialize;

pub struct Msg<'a> {
    pub role: &'a str, // "system" | "user" | "assistant"
    pub content: &'a str,
}

#[derive(Default)]
pub struct Opts<'a> {
    pub model: &'a str, // already resolved (caller substitutes the default)
    pub temperature: u32,
    pub max_tokens: u32,
    pub stop: Vec<String>,
    pub seed: u64,
}

pub struct Parsed {
    pub text: String,
    pub finish_reason: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    BadResponse(String),
    NoContent,
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// `temperature` is milli-units (700 -> 0.7); zero optionals are left to the server's defaults.
pub fn chat_body(messages: &[Msg], opts: &Opts) -> String {
    let system: Vec<&str> =
        messages.iter().filter(|m| m.role == "system").map(|m| m.content).collect();
    let system = (!system.is_empty()).then(|| system.join("\n\n"));
    let turns: Vec<Message> = messages
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| {
            let role = if m.role == "assistant" { Role::Assistant } else { Role::User };
            Message::new(role, vec![Part::Text(m.content.to_string())])
        })
        .collect();
    let req = Request {
        system: system.as_deref(),
        max_tokens: (opts.max_tokens > 0).then_some(opts.max_tokens),
        temperature: (opts.temperature > 0).then(|| opts.temperature as f64 / 1000.0),
        stop: &opts.stop,
        seed: (opts.seed > 0).then_some(opts.seed),
        ..Request::new(opts.model, &turns)
    };
    openai::body(&req).to_string()
}

pub fn embed_body(text: &str, model: &str) -> String {
    format!("{{\"model\":{},\"input\":{}}}", json_str(model), json_str(text))
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

fn narrow(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

pub fn parse_completion(body: &[u8]) -> Result<Parsed, ParseError> {
    let reply = openai::parse(body).map_err(|e| ParseError::BadResponse(e.to_string()))?;
    if reply.text.is_empty() {
        return Err(ParseError::NoContent);
    }
    Ok(Parsed {
        text: reply.text,
        finish_reason: reply.finish.unwrap_or_else(|| "other".to_string()),
        model: reply.model,
        prompt_tokens: narrow(reply.usage.total_input()),
        completion_tokens: narrow(reply.usage.output),
    })
}

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
            Msg { role: "system", content: "You are a routing agent." },
            Msg { role: "user", content: "Summarize this ticket." },
        ];
        let opts = Opts {
            model: "gemini-2.5-flash",
            temperature: 700,
            max_tokens: 256,
            ..Default::default()
        };
        let body = chat_body(&msgs, &opts);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["model"], "gemini-2.5-flash");
        assert_eq!(v["messages"][0]["role"], "system");
        assert_eq!(v["messages"][1]["role"], "user");
        assert_eq!(v["messages"][1]["content"], "Summarize this ticket.");
        assert_eq!(v["temperature"], 0.7);
        assert_eq!(v["max_tokens"], 256);
    }

    #[test]
    fn chat_body_omits_unset_optionals() {
        let msgs = [Msg { role: "user", content: "hi" }];
        let opts = Opts { model: "m", ..Default::default() };
        let v: serde_json::Value = serde_json::from_str(&chat_body(&msgs, &opts)).unwrap();
        assert!(v.get("temperature").is_none());
        assert!(v.get("max_tokens").is_none());
        assert!(v.get("seed").is_none());
    }

    #[test]
    fn embed_body_shapes_a_valid_request() {
        let v: serde_json::Value =
            serde_json::from_str(&embed_body("vector query", "text-embedding-3-small")).unwrap();
        assert_eq!(v["model"], "text-embedding-3-small");
        assert_eq!(v["input"], "vector query");
    }
}
