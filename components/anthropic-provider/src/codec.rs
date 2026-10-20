//! Messages-API policy, apart from the bindings so it unit-tests on the host (wire format: ADR-0116).
//! Messages are written as block lists because tools/claude-shim.mjs reads only that form.

use holon_llm::{anthropic, Message, Part, Request, Role};

pub struct Msg<'a> {
    pub role: &'a str,
    pub content: &'a str,
}

/// No `seed`: Anthropic's API has none, and carrying one would imply reproducibility this provider cannot give.
pub struct Opts<'a> {
    pub model: &'a str,
    pub temperature: u32, // milli-units, 700 -> 0.7
    pub max_tokens: u32,
    pub stop: Vec<String>,
}

pub struct Parsed {
    pub text: String,
    pub finish_reason: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

pub enum ParseError {
    BadResponse(String),
    NoContent,
}

enum TempSupport {
    Ranged {
        max_milli: u32,
    },
    /// Sending it is a 400 — the 5-generation models.
    Deprecated,
}

/// Defaults to Deprecated: a rejected temperature is a 400 that kills the run, withholding it only forfeits a knob.
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

/// An unknown role becomes `user`: dropping it would silently lose a turn.
/// The system block is a cache breakpoint: it is identical across every branch and repair.
pub fn messages_body(messages: &[Msg], opts: &Opts) -> String {
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
        max_tokens: Some(opts.max_tokens),
        // Sent only where accepted, clamped to the tier's range, so a repair can raise it without a 400.
        temperature: match temp_support(opts.model) {
            TempSupport::Ranged { max_milli } => {
                Some(opts.temperature.min(max_milli) as f64 / 1000.0)
            }
            TempSupport::Deprecated => None,
        },
        stop: &opts.stop,
        cache: true,
        blocks: true,
        ..Request::new(opts.model, &turns)
    };
    anthropic::body(&req).to_string()
}

fn narrow(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Anthropic sometimes answers a 200 with an error envelope when throttling, so it is checked on success too.
pub fn parse_completion(body: &[u8]) -> Result<Parsed, ParseError> {
    let reply = anthropic::parse(body).map_err(|e| ParseError::BadResponse(e.to_string()))?;
    if reply.text.is_empty() {
        // A thinking model can spend the whole budget thinking; say so rather than "returned nothing".
        if reply.hit_token_limit() {
            return Err(ParseError::BadResponse(
                "the model used its entire max-tokens budget before writing any text \
                 (a thinking model on a large task) — raise `anthropic:max-tokens`"
                    .into(),
            ));
        }
        return Err(ParseError::NoContent);
    }
    Ok(Parsed {
        text: reply.text,
        finish_reason: reply.finish.unwrap_or_else(|| "other".to_string()),
        model: reply.model,
        // Cache reads (~10%) and writes (~125%) counted at par make the cost an upper bound, safe for a budget.
        prompt_tokens: narrow(reply.usage.total_input()),
        completion_tokens: narrow(reply.usage.output),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(messages: &[Msg], max_tokens: u32) -> serde_json::Value {
        let opts = Opts { model: "claude-x", temperature: 200, max_tokens, stop: vec![] };
        serde_json::from_str(&messages_body(messages, &opts)).unwrap()
    }

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

    #[test]
    fn max_tokens_is_always_present() {
        let v = body(&[Msg { role: "user", content: "hi" }], 4096);
        assert_eq!(v["max_tokens"], 4096);
        assert!(
            v.get("temperature").is_none(),
            "temperature is deprecated on the 5-gen models, so never sent"
        );
        assert!(v.get("seed").is_none(), "there is no seed on this API");
    }

    fn body_with(model: &str, temp_milli: u32) -> serde_json::Value {
        let opts = Opts { model, temperature: temp_milli, max_tokens: 16, stop: vec![] };
        serde_json::from_str(&messages_body(&[Msg { role: "user", content: "hi" }], &opts)).unwrap()
    }

    #[test]
    fn temperature_is_sent_only_to_a_model_that_accepts_it() {
        let ok = body_with("claude-haiku-4-5-20251001", 500);
        assert_eq!(ok["temperature"], 0.5, "0.5 as a JSON float, not 500 milli-units");
        let dep = body_with("claude-opus-5", 500);
        assert!(dep.get("temperature").is_none(), "deprecated on 5-gen — never sent");
        assert!(body_with("some-vendor/model", 500).get("temperature").is_none());
    }

    #[test]
    fn an_escalated_temperature_is_clamped_to_the_range() {
        let v = body_with("claude-haiku-4-5-20251001", 4000);
        assert_eq!(v["temperature"], 1.0, "1000 milli is the ceiling");
    }

    #[test]
    fn a_top_level_cache_control_turns_on_automatic_caching() {
        let v = body(&[Msg { role: "user", content: "hi" }], 16);
        assert_eq!(
            v["cache_control"]["type"], "ephemeral",
            "automatic caching is enabled at the request level"
        );
        assert!(v["messages"][0]["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn cached_input_tokens_fold_into_the_prompt_total() {
        // input_tokens is only the post-breakpoint tail; the prompt total sums all three.
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

    #[test]
    fn non_text_blocks_are_skipped() {
        let body = br#"{"content":[{"type":"thinking","text":"hmm"},{"type":"text","text":"real"}],"usage":{"input_tokens":1,"output_tokens":1}}"#;
        assert_eq!(parse_completion(body).ok().unwrap().text, "real");
    }

    #[test]
    fn an_error_envelope_on_a_200_is_still_an_error() {
        let body =
            br#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert!(
            matches!(parse_completion(body), Err(ParseError::BadResponse(m)) if m.contains("Overloaded"))
        );
    }

    #[test]
    fn empty_content_is_no_content() {
        assert!(matches!(parse_completion(br#"{"content":[]}"#), Err(ParseError::NoContent)));
    }

    #[test]
    fn bad_json_is_bad_response() {
        assert!(matches!(parse_completion(b"not json"), Err(ParseError::BadResponse(_))));
    }

    /// Pins the bytes tools/claude-shim.mjs writes; zeroed usage because `claude -p` reports no counts.
    #[test]
    fn the_claude_code_shims_response_parses() {
        let shim = br#"{"type":"message","role":"assistant","model":"claude-haiku-4-5-20251001",
            "content":[{"type":"text","text":"PONG\n"}],"stop_reason":"end_turn",
            "usage":{"input_tokens":0,"output_tokens":0}}"#;

        let Ok(p) = parse_completion(shim) else {
            panic!("the shim's response must parse");
        };
        assert_eq!(p.text, "PONG\n");
        assert_eq!(p.finish_reason, "end_turn");
        assert_eq!(p.model, "claude-haiku-4-5-20251001");
        assert_eq!(p.prompt_tokens, 0, "the shim reports no counts rather than guessing");
        assert_eq!(p.completion_tokens, 0);
    }

    /// A crashed `claude -p` is transient (529), so the shim's envelope must parse as a retryable error.
    #[test]
    fn the_shims_error_envelope_is_recognised_as_an_api_error() {
        let shim = br#"{"type":"error","error":{"type":"overloaded_error",
            "message":"claude -p exceeded 540000ms"}}"#;

        match parse_completion(shim) {
            Err(ParseError::BadResponse(m)) => {
                assert!(m.contains("claude -p exceeded"), "the cause must survive: {m}");
            }
            Err(ParseError::NoContent) => panic!("the error envelope read as empty content"),
            Ok(_) => panic!("an error envelope parsed as a successful completion"),
        }
    }
}
