//! `mock-provider` — `llm:inference`, scripted.
//!
//! ## The script
//!
//! `mock-script` is JSON: an ordered list of rules, first match wins.
//!
//! ```json
//! {
//!   "rules": [
//!     { "when": "add a cache",  "seed": 1, "text": "diff for branch one" },
//!     { "when": "add a cache",  "seed": 2, "text": "diff for branch two" },
//!     { "when": "add a cache",              "text": "the fallback diff" },
//!     { "when": "explode",      "error": "provider-denied", "detail": "429" },
//!     { "when": "*",            "text": "I do not know." }
//!   ]
//! }
//! ```
//!
//! `when` is a substring of the whole conversation, or `"*"` for anything.
//! `seed` matches `options.seed` when present — which is how N branches asking
//! the SAME question get N different answers. The seed field already exists in
//! the contract for reproducibility, so nothing had to be invented for it, and a
//! swarm that varies its branches by seed varies its mock the same way.
//!
//! ## Delay, because latency changes the shape of everything
//!
//! `"delay_ms"` on a rule makes it wait before answering. A mock that returns in
//! microseconds turns a stress test into a measurement of the harness: real
//! inference takes hundreds of milliseconds to seconds, and THAT is what decides
//! how many branches are in flight at once, how long an environment is held open,
//! and whether anything queues behind anything else.
//!
//! ## Failure modes are the point
//!
//! A rule may return an error instead of text. The interesting paths through a
//! graph loop are the ones where inference is rate-limited, returns nothing, or
//! answers with something unusable — and those are precisely the paths a real
//! provider will not produce on demand. `"error"` names any `infer-error` case.
//!
//! ## Embeddings that actually have similarity structure
//!
//! `embed` could return a hash and be useless: every test of retrieval or ranking
//! would then be testing nothing, because unrelated texts would be equidistant.
//! Instead this is a **hashing vectoriser** — tokens hashed into dimensions, the
//! vector L2-normalised — so texts sharing words really are closer together. It
//! is crude and it is deterministic and free, which makes retrieval logic
//! testable without paying anyone.
//!
//! It is not semantic: "car" and "automobile" are orthogonal here and would not
//! be to a real model. Anything asserting *semantic* similarity is asserting
//! something this cannot provide, and should be recorded from a real provider
//! instead.

#[allow(warnings)]
mod bindings;

use bindings::exports::llm::inference::inference::{
    Completion, Guest, InferError, Message, Options, Role, Usage,
};
use bindings::wasi::clocks::monotonic_clock;
use bindings::wasi::config::store as config;

use sha2::{Digest, Sha256};

struct Component;

/// Deliberately small. A vector this size is enough to show that similar texts
/// are closer than dissimilar ones, and small enough to eyeball in a failure
/// message.
const EMBED_DIM: usize = 64;

fn cfg(key: &str, default: &str) -> String {
    config::get(key).ok().flatten().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
}

fn model_name() -> String {
    cfg("mock-model", "mock-1")
}

/// Whether this deployment claims an embedding model, `mock-embeddings=false` to
/// say it does not.
///
/// Not a feature — a test surface. `anthropic-provider` refuses `embed` because
/// Anthropic has no embeddings endpoint, and a caller that degrades to sparse
/// retrieval when `describe()` says so (`knowledge:memory` does) has no other way
/// to exercise that path without an API key and a live vendor. Defaults to
/// available, so nothing that already links this changes.
fn embeddings_available() -> bool {
    cfg("mock-embeddings", "true") != "false"
}

/// Everything the caller said, joined — what `when` matches against.
fn conversation(messages: &[Message]) -> String {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            format!("{role}: {}", m.content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn error_for(name: &str, detail: &str) -> InferError {
    match name {
        "invalid-request" => InferError::InvalidRequest(detail.to_string()),
        "provider-denied" => InferError::ProviderDenied(detail.to_string()),
        "provider-unavailable" => InferError::ProviderUnavailable(detail.to_string()),
        "bad-response" => InferError::BadResponse(detail.to_string()),
        "no-content" => InferError::NoContent,
        // An unknown error name is a mistake in the script, and saying so is far
        // more useful than quietly picking one — a test asserting on
        // `provider-denied` would otherwise pass or fail for the wrong reason.
        other => InferError::InvalidRequest(format!("mock script names no such error: {other:?}")),
    }
}

/// Find the first rule that matches. Order is significant, so a specific rule can
/// sit above a general one.
fn select(
    script: &serde_json::Value,
    text: &str,
    seed: u64,
) -> Result<serde_json::Value, InferError> {
    let rules = script["rules"].as_array().cloned().unwrap_or_default();
    if rules.is_empty() {
        return Err(InferError::InvalidRequest(
            "mock-script has no rules — this provider answers nothing until it is scripted".into(),
        ));
    }
    for rule in rules {
        let when = rule["when"].as_str().unwrap_or("*");
        if when != "*" && !text.contains(when) {
            continue;
        }
        // A rule naming a seed only matches that seed. A rule with no seed
        // matches any, which is what makes it a fallback.
        if let Some(want) = rule["seed"].as_u64() {
            if want != seed {
                continue;
            }
        }
        return Ok(rule);
    }
    // Silence would be indistinguishable from an empty completion, and a test
    // whose prompt drifted out of its script should say so loudly.
    Err(InferError::InvalidRequest(format!(
        "no mock rule matches this conversation (seed {seed}); the script is out of date"
    )))
}

/// `default_model` is passed IN rather than read from config, so this stays a
/// pure function: a native unit test cannot call a WASI import, and a mock whose
/// own logic is untestable would be a poor foundation for testing everything else.
fn completion_from(
    rule: &serde_json::Value,
    prompt_len: usize,
    default_model: &str,
) -> Result<Completion, InferError> {
    // Before the answer AND before an error, because a provider that refuses you
    // still made you wait for the refusal — and a retry loop that assumes
    // failures are free is a retry loop that will surprise somebody.
    if let Some(ms) = rule["delay_ms"].as_u64() {
        if ms > 0 {
            monotonic_clock::subscribe_duration(ms * 1_000_000).block();
        }
    }
    if let Some(name) = rule["error"].as_str() {
        return Err(error_for(name, rule["detail"].as_str().unwrap_or_default()));
    }
    let text = rule["text"].as_str().unwrap_or_default().to_string();
    if text.is_empty() {
        return Err(InferError::NoContent);
    }
    Ok(Completion {
        text: text.clone(),
        finish_reason: rule["finish"].as_str().unwrap_or("stop").to_string(),
        model: rule["model"].as_str().unwrap_or(default_model).to_string(),
        // Token counts are made up but PROPORTIONAL, so a test of the fuel
        // accounting sees a bigger answer cost more. A constant would make every
        // budget test pass for the wrong reason.
        usage: Usage {
            prompt_tokens: (prompt_len / 4) as u32,
            completion_tokens: (text.len() / 4) as u32,
        },
    })
}

/// Split on anything that is not alphanumeric, lowercased.
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// A hashing vectoriser: each token lands in a dimension by its hash, weighted by
/// sqrt of its frequency, and the vector is L2-normalised so cosine similarity is
/// a dot product.
fn embed_text(text: &str) -> Vec<f32> {
    let mut v = vec![0f32; EMBED_DIM];
    for t in tokens(text) {
        let mut h = Sha256::new();
        h.update(t.as_bytes());
        let d = h.finalize();
        let idx = (u16::from_le_bytes([d[0], d[1]]) as usize) % EMBED_DIM;
        // The sign comes from a second byte, so unrelated tokens can cancel
        // rather than all pushing the same way and making everything similar.
        let sign = if d[2] & 1 == 0 { 1.0 } else { -1.0 };
        v[idx] += sign;
    }
    // sqrt-tf, then normalise.
    for x in v.iter_mut() {
        *x = x.signum() * x.abs().sqrt();
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

fn script() -> Result<serde_json::Value, InferError> {
    let raw = cfg("mock-script", "");
    if raw.is_empty() {
        return Err(InferError::InvalidRequest(
            "no mock-script configured — this provider is deliberately useless unscripted".into(),
        ));
    }
    serde_json::from_str(&raw)
        .map_err(|e| InferError::InvalidRequest(format!("mock-script is not JSON: {e}")))
}

impl Guest for Component {
    fn chat(messages: Vec<Message>, opts: Options) -> Result<Completion, InferError> {
        if messages.is_empty() {
            return Err(InferError::InvalidRequest("no messages".into()));
        }
        let text = conversation(&messages);
        let rule = select(&script()?, &text, opts.seed)?;
        completion_from(&rule, text.len(), &model_name())
    }

    fn complete(prompt: String, system: String, opts: Options) -> Result<Completion, InferError> {
        let mut messages = Vec::new();
        if !system.is_empty() {
            messages.push(Message { role: Role::System, content: system });
        }
        messages.push(Message { role: Role::User, content: prompt });
        Self::chat(messages, opts)
    }

    fn embed(text: String, _opts: Options) -> Result<Vec<f32>, InferError> {
        if !embeddings_available() {
            // A provider that says it cannot embed must also refuse to, or the
            // degraded path is only tested in the callers that bother to ask.
            return Err(InferError::InvalidRequest(
                "mock-embeddings is off — this deployment has no embedding model".into(),
            ));
        }
        if text.is_empty() {
            return Err(InferError::InvalidRequest("empty text".into()));
        }
        Ok(embed_text(&text))
    }

    fn describe() -> (String, bool) {
        (model_name(), embeddings_available())
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn script_of(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    /// The property the whole component exists for.
    #[test]
    fn the_same_question_gets_the_same_answer_every_time() {
        let s = script_of(r#"{"rules":[{"when":"*","text":"always this"}]}"#);
        let a = completion_from(&select(&s, "anything", 0).unwrap(), 100, "mock-1").unwrap();
        let b = completion_from(&select(&s, "anything", 0).unwrap(), 100, "mock-1").unwrap();
        assert_eq!(a.text, b.text);
        assert_eq!(a.usage.completion_tokens, b.usage.completion_tokens);
    }

    /// How N branches asking one question get N different candidates — using the
    /// seed the contract already has, rather than a mechanism invented here.
    #[test]
    fn the_seed_is_what_makes_branches_differ() {
        let s = script_of(
            r#"{"rules":[
                {"when":"cache","seed":1,"text":"branch one"},
                {"when":"cache","seed":2,"text":"branch two"},
                {"when":"cache","text":"fallback"}
            ]}"#,
        );
        let pick = |seed| {
            completion_from(&select(&s, "add a cache", seed).unwrap(), 10, "mock-1").unwrap().text
        };
        assert_eq!(pick(1), "branch one");
        assert_eq!(pick(2), "branch two");
        // A seed with no rule of its own falls through to the seedless rule.
        assert_eq!(pick(9), "fallback");
    }

    /// Order matters, so a specific rule can sit above a general one.
    #[test]
    fn the_first_matching_rule_wins() {
        let s = script_of(
            r#"{"rules":[{"when":"specific","text":"narrow"},{"when":"*","text":"broad"}]}"#,
        );
        assert_eq!(
            completion_from(&select(&s, "a specific thing", 0).unwrap(), 1, "mock-1").unwrap().text,
            "narrow"
        );
        assert_eq!(
            completion_from(&select(&s, "something else", 0).unwrap(), 1, "mock-1").unwrap().text,
            "broad"
        );
    }

    /// The paths a real provider will not produce on demand.
    #[test]
    fn failure_modes_can_be_scripted() {
        let s = script_of(
            r#"{"rules":[
                {"when":"explode","error":"provider-denied","detail":"429 slow down"},
                {"when":"empty","text":""},
                {"when":"*","text":"fine"}
            ]}"#,
        );
        match completion_from(&select(&s, "explode now", 0).unwrap(), 1, "mock-1") {
            Err(InferError::ProviderDenied(m)) => assert!(m.contains("429")),
            other => panic!("expected a denial: {other:?}"),
        }
        // An empty text is `no-content`, which is a real thing providers do and a
        // distinct case the loop has to handle.
        assert!(matches!(
            completion_from(&select(&s, "empty please", 0).unwrap(), 1, "mock-1"),
            Err(InferError::NoContent)
        ));
    }

    /// A prompt that drifted out of its script must fail loudly, not silently
    /// return nothing — otherwise a test keeps passing while testing the mock.
    #[test]
    fn an_unmatched_prompt_is_an_error_not_silence() {
        let s = script_of(r#"{"rules":[{"when":"only this","text":"ok"}]}"#);
        assert!(select(&s, "something the script never anticipated", 0).is_err());
        // And a script with no rules at all says so, rather than answering.
        assert!(select(&script_of(r#"{"rules":[]}"#), "hello", 0).is_err());
    }

    /// Made-up token counts, but proportional — so a fuel test sees a longer
    /// answer cost more. A constant would let every budget assertion pass for the
    /// wrong reason.
    #[test]
    fn a_longer_answer_costs_more() {
        let s = script_of(
            r#"{"rules":[{"when":"short","text":"hi"},{"when":"long","text":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}"#,
        );
        let short = completion_from(&select(&s, "short", 0).unwrap(), 10, "mock-1").unwrap();
        let long = completion_from(&select(&s, "long", 0).unwrap(), 10, "mock-1").unwrap();
        assert!(long.usage.completion_tokens > short.usage.completion_tokens);
    }

    /// An embedding with real similarity structure, so retrieval and ranking can
    /// be tested at all. A pure hash would make every text equidistant and every
    /// such test vacuous.
    #[test]
    fn similar_texts_embed_closer_than_dissimilar_ones() {
        let a = embed_text("the cache stores derived artifacts");
        let b = embed_text("derived artifacts live in the cache");
        let c = embed_text("badgers forage at dusk in wet grass");

        assert!((cosine(&a, &a) - 1.0).abs() < 1e-5, "a normalised vector dots to 1 with itself");
        assert!(
            cosine(&a, &b) > cosine(&a, &c),
            "shared words should mean closer vectors: ab={} ac={}",
            cosine(&a, &b),
            cosine(&a, &c)
        );
        // Deterministic, which is the whole point.
        assert_eq!(embed_text("stable"), embed_text("stable"));
        assert_eq!(a.len(), EMBED_DIM);
    }
}
