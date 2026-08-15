//! Pure Anthropic Messages-API request-building + response-parsing, decoupled
//! from the WIT bindings so it is unit-testable on the host (`cargo test`).
//!
//! Everything here is deterministic and dependency-light (serde_json only). The
//! HTTP plumbing, config and secret live in `lib.rs`; this is just the codec —
//! and the codec is where Anthropic differs from OpenAI, so it is the part worth
//! testing in isolation.
//!
//! The three shape differences from an OpenAI request, each encoded below:
//!
//! * the SYSTEM prompt is a top-level string, not a message with role `system`,
//!   so system turns are lifted out of `messages`;
//! * `max_tokens` is REQUIRED — a request without it is a 400, so the caller
//!   always resolves one;
//! * the response is a list of `content` BLOCKS, not `choices`, and usage counts
//!   are `input_tokens`/`output_tokens`.

use serde::Deserialize;

/// A chat message (plain mirror of the WIT `message`).
pub struct Msg<'a> {
    pub role: &'a str, // "system" | "user" | "assistant"
    pub content: &'a str,
}

/// Completion tunables. No `seed`: Anthropic's API has none, and carrying a field
/// the wire ignores would imply a reproducibility this provider cannot give.
pub struct Opts<'a> {
    pub model: &'a str,   // already resolved (non-empty)
    pub temperature: u32, // milli-units, 700 -> 0.7
    pub max_tokens: u32,  // already resolved to a positive value
    pub stop: Vec<String>,
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

/// Whether a model accepts a `temperature`, as a tagged union rather than a
/// bare bool, so a future tier that caps at a different range says so in one
/// place instead of a `< 0.7` scattered through the caller.
enum TempSupport {
    /// Accepts temperature up to `max_milli` milli-units (1000 == 1.0).
    Ranged { max_milli: u32 },
    /// Sending it is a 400 — the 5-generation models.
    Deprecated,
}

/// Classify a model id. The default is DEPRECATED, deliberately: sending
/// temperature to a model that rejects it is a hard 400 that kills the run,
/// while withholding it from one that would accept it only forfeits a knob. So a
/// model earns temperature by being on the known-accepting list, not by failing
/// to match a deny-list that a new 5-gen name would slip through.
fn temp_support(model: &str) -> TempSupport {
    let m = model.to_ascii_lowercase();
    let accepts = m.contains("haiku-4-5")
        || m.contains("sonnet-4-5")
        || m.contains("sonnet-4-6")
        || m.contains("opus-4-1")
        || m.contains("haiku-3")
        || m.contains("sonnet-3");
    if accepts {
        TempSupport::Ranged { max_milli: 1000 }
    } else {
        TempSupport::Deprecated
    }
}

/// Build the `/v1/messages` request body.
///
/// System messages are concatenated into the top-level `system` field and
/// removed from `messages`; everything else stays a user/assistant turn. An
/// unknown role is treated as `user`, because the alternative — dropping it —
/// silently loses a turn the caller meant to send.
/// The ephemeral cache-breakpoint marker, appended inside a content block.
/// Everything in the request up to and including a marked block is cached and,
/// on a later request whose prefix is byte-identical, read back at ~10% of the
/// price. We keep ONE explicit breakpoint — on the system block — because the
/// system + files prefix is identical across every branch AND every repair, so
/// it must cache independently of the message tail that a repair changes. The
/// growing message tail is left to automatic caching (the top-level field
/// below), which the docs call the simplest way and which moves its own
/// breakpoint to the last cacheable block for us.
const CACHE: &str = ",\"cache_control\":{\"type\":\"ephemeral\"}";

pub fn messages_body(messages: &[Msg], opts: &Opts) -> String {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut user_turns: Vec<&Msg> = Vec::new();
    for m in messages {
        if m.role == "system" {
            system_parts.push(m.content);
        } else {
            user_turns.push(m);
        }
    }

    // The turns are plain text blocks; the message tail's breakpoint is handled
    // by automatic caching (the top-level `cache_control` below), which places
    // and advances its own breakpoint on the last cacheable block. Hand-marking
    // the last turn here would only duplicate what automatic caching does.
    let turns: Vec<String> = user_turns
        .iter()
        .map(|m| {
            let role = if m.role == "assistant" { "assistant" } else { "user" };
            format!(
                "{{\"role\":\"{role}\",\"content\":[{{\"type\":\"text\",\"text\":{}}}]}}",
                json_str(m.content)
            )
        })
        .collect();

    let mut parts = vec![
        format!("\"model\":{}", json_str(opts.model)),
        // Required. Resolved by the caller, never 0 here.
        format!("\"max_tokens\":{}", opts.max_tokens),
        format!("\"messages\":[{}]", turns.join(",")),
        // Automatic caching: one top-level breakpoint that the system places on
        // the last cacheable block and moves forward as the conversation grows.
        // The docs' simplest enable, and it composes with the explicit system
        // breakpoint (they take separate breakpoint slots).
        "\"cache_control\":{\"type\":\"ephemeral\"}".to_string(),
    ];
    if !system_parts.is_empty() {
        // The system prompt is identical across every branch and every attempt,
        // so it is its own cache breakpoint — written once, read forever after.
        parts.push(format!(
            "\"system\":[{{\"type\":\"text\",\"text\":{}{CACHE}}}]",
            json_str(&system_parts.join("\n\n"))
        ));
    }
    // Whether `temperature` is sent depends on the model. The 5-generation models
    // deprecated it and answer 400 if it carries; earlier tiers accept it. Sent
    // only where accepted, clamped to the tier's range — so a repair can turn the
    // knob up (the writer escalates it) without a 400 killing the whole run.
    if let TempSupport::Ranged { max_milli } = temp_support(opts.model) {
        let milli = opts.temperature.min(max_milli);
        // milli-units to a JSON float: 200 -> 0.2, 1000 -> 1. Rust's shortest
        // round-trip formatting keeps it a clean decimal.
        parts.push(format!("\"temperature\":{}", milli as f64 / 1000.0));
    }
    if !opts.stop.is_empty() {
        let stops: Vec<String> = opts.stop.iter().map(|s| json_str(s)).collect();
        parts.push(format!("\"stop_sequences\":[{}]", stops.join(",")));
    }
    format!("{{{}}}", parts.join(","))
}

#[derive(Deserialize)]
struct MsgResp {
    #[serde(default)]
    content: Vec<Block>,
    #[serde(default)]
    model: String,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<UsageResp>,
    #[serde(default)]
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    error: Option<ApiError>,
}
#[derive(Deserialize)]
struct Block {
    #[serde(default)]
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}
#[derive(Deserialize)]
struct UsageResp {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    // With caching on, `input_tokens` counts ONLY the tokens after the last
    // breakpoint; the cached prefix is reported here instead. Summing all three
    // gives the true input the model processed — without them a cached run looks
    // ~free and the wallet under-counts what it spent.
    #[serde(default)]
    cache_creation_input_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
}
#[derive(Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
}

/// Parse a `/v1/messages` response into the domain completion.
///
/// Anthropic answers a 200 with `{"type":"error",...}` in some throttling cases,
/// so an error envelope is checked even on a success status rather than trusting
/// the code alone.
pub fn parse_completion(body: &[u8]) -> Result<Parsed, ParseError> {
    let parsed: MsgResp = serde_json::from_slice(body)
        .map_err(|e| ParseError::BadResponse(format!("messages json: {e}")))?;

    if parsed.kind == "error" {
        let m = parsed.error.map(|e| e.message).unwrap_or_else(|| "unknown error".into());
        return Err(ParseError::BadResponse(format!("api error: {m}")));
    }

    // Concatenate the text of every text block. A response can carry non-text
    // blocks (tool use, thinking); those are skipped rather than stringified.
    let text: String = parsed
        .content
        .iter()
        .filter(|b| b.kind == "text")
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() {
        // A thinking model on a real task can spend the WHOLE budget thinking and
        // never reach a text block: `content` is `["thinking"]` and `stop_reason`
        // is `max_tokens`. Measured on claude-sonnet-5 at 4096 — which is a
        // perfectly good budget for a non-thinking model, so the failure arrives
        // the day someone changes the model and nothing else. "The model returned
        // nothing" sends that person to the wrong place; this says where to look.
        if parsed.stop_reason.as_deref() == Some("max_tokens") {
            return Err(ParseError::BadResponse(
                "the model used its entire max-tokens budget before writing any text \
                 (a thinking model on a large task) — raise `anthropic:max-tokens`"
                    .into(),
            ));
        }
        return Err(ParseError::NoContent);
    }

    let usage = parsed.usage.unwrap_or(UsageResp {
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
    });
    // True total input = post-breakpoint + freshly-cached + cache-read. Reported
    // as prompt_tokens so cost/budget see the real work. Cache reads are billed
    // at ~10% and writes at ~125%, so counting them at par makes the dollar cost
    // an UPPER bound — safe for a budget ceiling, which should never undershoot.
    let prompt_tokens = usage
        .input_tokens
        .saturating_add(usage.cache_creation_input_tokens)
        .saturating_add(usage.cache_read_input_tokens);
    Ok(Parsed {
        text,
        finish_reason: parsed.stop_reason.unwrap_or_else(|| "other".to_string()),
        model: parsed.model,
        prompt_tokens,
        completion_tokens: usage.output_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(messages: &[Msg], max_tokens: u32) -> serde_json::Value {
        let opts = Opts { model: "claude-x", temperature: 200, max_tokens, stop: vec![] };
        serde_json::from_str(&messages_body(messages, &opts)).unwrap()
    }

    /// The system turn must leave `messages` and become the top-level field, or
    /// Anthropic rejects a `system` role with a 400.
    #[test]
    fn a_system_message_becomes_the_top_level_system_field() {
        let v = body(
            &[
                Msg { role: "system", content: "You write code." },
                Msg { role: "user", content: "make it 42" },
            ],
            256,
        );
        assert_eq!(v["system"][0]["text"], "You write code.");
        assert_eq!(v["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(v["messages"].as_array().unwrap().len(), 1, "only the user turn remains");
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"][0]["text"], "make it 42");
    }

    #[test]
    fn several_system_turns_are_joined() {
        let v = body(
            &[
                Msg { role: "system", content: "A." },
                Msg { role: "system", content: "B." },
                Msg { role: "user", content: "go" },
            ],
            16,
        );
        assert_eq!(v["system"][0]["text"], "A.\n\nB.");
    }

    /// max_tokens is required and always present, because a request without it is
    /// a 400 from Anthropic.
    #[test]
    fn max_tokens_is_always_present() {
        let v = body(&[Msg { role: "user", content: "hi" }], 4096);
        assert_eq!(v["max_tokens"], 4096);
        assert!(v.get("temperature").is_none(), "temperature is deprecated on the 5-gen models, so never sent");
        assert!(v.get("seed").is_none(), "there is no seed on this API");
    }

    fn body_with(model: &str, temp_milli: u32) -> serde_json::Value {
        let opts = Opts { model, temperature: temp_milli, max_tokens: 16, stop: vec![] };
        serde_json::from_str(&messages_body(&[Msg { role: "user", content: "hi" }], &opts)).unwrap()
    }

    #[test]
    fn temperature_is_sent_only_to_a_model_that_accepts_it() {
        // Haiku 4.5 accepts it; a 5-gen model would 400, so it is withheld.
        let ok = body_with("claude-haiku-4-5-20251001", 500);
        assert_eq!(ok["temperature"], 0.5, "0.5 as a JSON float, not 500 milli-units");
        let dep = body_with("claude-opus-5", 500);
        assert!(dep.get("temperature").is_none(), "deprecated on 5-gen — never sent");
        // The safe default: an unknown model is treated as deprecated.
        assert!(body_with("some-vendor/model", 500).get("temperature").is_none());
    }

    #[test]
    fn an_escalated_temperature_is_clamped_to_the_range() {
        // The writer may ask for more than 1.0 as it escalates; it clamps to max.
        let v = body_with("claude-haiku-4-5-20251001", 4000);
        assert_eq!(v["temperature"], 1.0, "1000 milli is the ceiling");
    }

    #[test]
    fn a_top_level_cache_control_turns_on_automatic_caching() {
        let v = body(&[Msg { role: "user", content: "hi" }], 16);
        assert_eq!(v["cache_control"]["type"], "ephemeral", "automatic caching is enabled at the request level");
        // The message tail is a plain text block — automatic caching owns its breakpoint.
        assert!(v["messages"][0]["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn cached_input_tokens_fold_into_the_prompt_total() {
        // input_tokens is only the post-breakpoint tail; the cached prefix lives
        // in the two cache fields. The reported prompt total sums all three.
        let body = br#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":50,"cache_creation_input_tokens":0,"cache_read_input_tokens":1000,"output_tokens":7}}"#;
        let p = parse_completion(body).ok().unwrap();
        assert_eq!(p.prompt_tokens, 1050, "50 fresh + 1000 read from cache");
        assert_eq!(p.completion_tokens, 7);
    }

    #[test]
    fn content_is_escaped_and_round_trips() {
        let v = body(&[Msg { role: "user", content: "quote \" and \n newline" }], 16);
        assert_eq!(v["messages"][0]["content"][0]["text"], "quote \" and \n newline");
    }

    #[test]
    fn parse_reads_text_blocks_model_and_usage() {
        let body = br#"{"type":"message","model":"claude-x","stop_reason":"end_turn","content":[{"type":"text","text":"pub fn answer"},{"type":"text","text":"() -> u32 { 42 }"}],"usage":{"input_tokens":11,"output_tokens":7}}"#;
        let p = parse_completion(body).ok().unwrap();
        assert_eq!(p.text, "pub fn answer() -> u32 { 42 }", "text blocks concatenate");
        assert_eq!(p.finish_reason, "end_turn");
        assert_eq!(p.model, "claude-x");
        assert_eq!(p.prompt_tokens, 11);
        assert_eq!(p.completion_tokens, 7);
    }

    /// A non-text block (tool use, thinking) alongside text must not corrupt the
    /// answer.
    #[test]
    fn non_text_blocks_are_skipped() {
        let body = br#"{"content":[{"type":"thinking","text":"hmm"},{"type":"text","text":"real"}],"usage":{"input_tokens":1,"output_tokens":1}}"#;
        assert_eq!(parse_completion(body).ok().unwrap().text, "real");
    }

    #[test]
    fn an_error_envelope_on_a_200_is_still_an_error() {
        let body = br#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert!(matches!(parse_completion(body), Err(ParseError::BadResponse(m)) if m.contains("Overloaded")));
    }

    #[test]
    fn empty_content_is_no_content() {
        assert!(matches!(parse_completion(br#"{"content":[]}"#), Err(ParseError::NoContent)));
    }

    #[test]
    fn bad_json_is_bad_response() {
        assert!(matches!(parse_completion(b"not json"), Err(ParseError::BadResponse(_))));
    }
}
