//! `typesafe-provider` — the TypeSafe AI (Jev) side of the decision contract.
//!
//! Implements the vendor-agnostic `jev:decision/decision` by POSTing to
//! `https://api.typesafe.ai/v1/systemone` — Jev's single evaluation endpoint,
//! per docs.typesafe.ai/api. Jev is a "System One" decision engine: it
//! evaluates a `state` against typed question schemas (Choice, Score, Noul)
//! and returns discrete probabilities, never free-form text — so this is
//! deliberately NOT shaped like `openai-provider`'s chat-completion codec; see
//! `codec.rs` for the exact request/response wire shape.
//!
//! HTTP idiom (build OutgoingRequest -> write JSON body -> handle -> block ->
//! read full response body) mirrors `openai-provider`'s `post_json`, with
//! tighter timeouts: Jev is documented as ultra-low-latency, so a request that
//! hasn't answered in a few seconds is a `provider-unavailable`, not something
//! worth waiting minutes for the way a chat completion is.
//!
//! Config (wasi:config/store):
//!   typesafe:base-url  default "https://api.typesafe.ai"
//!   typesafe:model     default "jev-latest"
//!   typesafe:timeout   seconds to wait for the first response byte, and
//!                      between bytes after it (default 5)
//!
//! Secret (comp:secrets/reader):
//!   typesafe-api-key   bearer token, granted by reference in the manifest
//!
//! Unlike `openai-provider`, a missing key is not a supported "no-auth local
//! server" mode — Jev is always a hosted API — so a missing key is refused
//! before any request is sent, rather than sent unauthenticated and left to
//! come back as a 401.

#[allow(warnings)]
mod bindings;
mod codec;

use bindings::comp::secrets::reader as secrets;
use bindings::exports::jev::decision::decision::{
    AnswerKind, Answered, ChoiceRequest, ChoiceResult, DecisionError, GateRequest, GateResult,
    Guest, OptionScore, Question, QuestionKind, ScoreRequest, ScoreResult,
};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;

struct Component;

const DEFAULT_BASE: &str = "https://api.typesafe.ai";
const DEFAULT_MODEL: &str = "jev-latest";
/// Ultra-low-latency by design (the task's own framing) — a hosted decision
/// call that hasn't answered in a few seconds is unhealthy, not merely slow,
/// unlike `openai-provider`'s ten-minute chat budget.
const DEFAULT_TIMEOUT_SECS: u64 = 5;
const CONNECT_TIMEOUT_NS: u64 = 3_000_000_000; // 3s

// ---- config -------------------------------------------------------------

fn cfg(key: &str) -> Option<String> {
    config::get(key).ok().flatten().filter(|s| !s.is_empty())
}

fn base_url() -> String {
    cfg("typesafe:base-url").unwrap_or_else(|| DEFAULT_BASE.to_string())
}

fn model() -> String {
    cfg("typesafe:model").unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn timeout_ns() -> u64 {
    cfg("typesafe:timeout")
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .saturating_mul(1_000_000_000)
}

/// The bearer token, from the vault. `None` here is a caller/deployment
/// error — Jev has no unauthenticated mode — so callers check this before
/// making a request rather than sending one that can only come back denied.
fn api_key() -> Option<String> {
    match secrets::get("typesafe-api-key") {
        Ok(Some(s)) => secrets::reveal(&s).ok().filter(|v| !v.is_empty()),
        _ => None,
    }
}

// ---- http ---------------------------------------------------------------

fn parse_url(url: &str) -> Result<(Scheme, String, String), DecisionError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(DecisionError::ProviderUnavailable(format!("bad url scheme: {url}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), "/".to_string()),
    };
    Ok((scheme, authority, path))
}

/// POST `body` as application/json to `base_url + path` with a bearer token.
/// Returns (status, response-body-bytes). Network failures map to
/// `provider-unavailable`.
fn post_json(path: &str, key: &str, body: &[u8]) -> Result<(u16, Vec<u8>), DecisionError> {
    let url = format!("{}{}", base_url().trim_end_matches('/'), path);
    let (scheme, authority, full_path) = parse_url(&url)?;

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let _ = headers.set("authorization", &[format!("Bearer {key}").into_bytes()]);

    let req = OutgoingRequest::new(headers);
    let net = |m: &str| DecisionError::ProviderUnavailable(m.to_string());
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&full_path)).map_err(|_| net("set path"))?;

    {
        let out = req.body().map_err(|_| net("body"))?;
        {
            let stream = out.write().map_err(|_| net("write stream"))?;
            // blocking_write_and_flush caps at 4096 bytes/call — chunk it.
            for chunk in body.chunks(4096) {
                stream
                    .blocking_write_and_flush(chunk)
                    .map_err(|e| net(&format!("body write: {e:?}")))?;
            }
        }
        OutgoingBody::finish(out, None).map_err(|_| net("finish body"))?;
    }

    let opts = RequestOptions::new();
    let read_ns = timeout_ns();
    let _ = opts.set_connect_timeout(Some(CONNECT_TIMEOUT_NS));
    let _ = opts.set_first_byte_timeout(Some(read_ns));
    let _ = opts.set_between_bytes_timeout(Some(read_ns));

    let future = outgoing_handler::handle(req, Some(opts))
        .map_err(|e| DecisionError::ProviderUnavailable(format!("http handle: {e:?}")))?;
    future.subscribe().block();
    let resp = future
        .get()
        .ok_or_else(|| net("no response"))?
        .map_err(|_| net("response taken"))?
        .map_err(|e| DecisionError::ProviderUnavailable(format!("http: {e:?}")))?;

    let status = resp.status();
    let mut buf = Vec::new();
    if let Ok(incoming) = resp.consume() {
        if let Ok(stream) = incoming.stream() {
            loop {
                match stream.blocking_read(8192) {
                    Ok(c) if c.is_empty() => break,
                    Ok(c) => buf.extend_from_slice(&c),
                    Err(StreamError::Closed) => break,
                    Err(_) => break,
                }
            }
        }
    }
    Ok((status, buf))
}

/// Map a non-2xx status to the right decision-error, carrying the body
/// snippet. Per docs.typesafe.ai/api: 401 (bad key), 422 (validation), 429
/// (rate limit), 529 (temporarily overloaded — Jev's own status, treated the
/// same as any other server-side unavailability).
fn status_error(status: u16, body: &[u8]) -> DecisionError {
    let snippet = String::from_utf8_lossy(body).chars().take(300).collect::<String>();
    match status {
        400 | 422 => DecisionError::InvalidRequest(snippet),
        401 | 403 | 429 => DecisionError::ProviderDenied(format!("{status}: {snippet}")),
        _ => DecisionError::ProviderUnavailable(format!("{status}: {snippet}")),
    }
}

fn parse_err(e: codec::ParseError) -> DecisionError {
    match e {
        codec::ParseError::BadResponse(m) => DecisionError::BadResponse(m),
    }
}

/// Every call needs the same key; the checks a request needs before it is
/// worth sending live here once.
fn ready(state: &str) -> Result<String, DecisionError> {
    if state.is_empty() {
        return Err(DecisionError::InvalidRequest("empty state".into()));
    }
    api_key().ok_or_else(|| DecisionError::ProviderDenied("missing typesafe-api-key".into()))
}

// ---- guest --------------------------------------------------------------

impl Guest for Component {
    fn choose(req: ChoiceRequest) -> Result<ChoiceResult, DecisionError> {
        if req.options.is_empty() {
            return Err(DecisionError::InvalidRequest("no options offered".into()));
        }
        let key = ready(&req.state)?;
        let body = codec::choose_body(&model(), &req.state, &req.instructions, &req.options);
        let (status, resp) = post_json("/v1/systemone", &key, body.as_bytes())?;
        if !(200..300).contains(&status) {
            return Err(status_error(status, &resp));
        }
        let p = codec::parse_choice(&resp, &req.options).map_err(parse_err)?;
        Ok(ChoiceResult {
            selected: p.selected,
            confidence: p.confidence,
            distribution: p
                .distribution
                .into_iter()
                .map(|(label, score)| OptionScore { label, score })
                .collect(),
            flat: p.flat,
        })
    }

    fn score(req: ScoreRequest) -> Result<ScoreResult, DecisionError> {
        if req.levels.len() < 2 {
            return Err(DecisionError::InvalidRequest("at least two levels are required".into()));
        }
        let key = ready(&req.state)?;
        let body = codec::score_body(&model(), &req.state, &req.instructions, &req.levels);
        let (status, resp) = post_json("/v1/systemone", &key, body.as_bytes())?;
        if !(200..300).contains(&status) {
            return Err(status_error(status, &resp));
        }
        let p = codec::parse_score(&resp, &req.levels).map_err(parse_err)?;
        Ok(ScoreResult {
            value: p.value,
            confidence: p.confidence,
            distribution: p
                .distribution
                .into_iter()
                .map(|(label, score)| OptionScore { label, score })
                .collect(),
        })
    }

    fn gate(req: GateRequest) -> Result<GateResult, DecisionError> {
        if req.instructions.is_empty() {
            return Err(DecisionError::InvalidRequest("no instructions given".into()));
        }
        let key = ready(&req.state)?;
        let body =
            codec::gate_body(&model(), &req.state, &req.instructions, &req.true_hint, &req.false_hint);
        let (status, resp) = post_json("/v1/systemone", &key, body.as_bytes())?;
        if !(200..300).contains(&status) {
            return Err(status_error(status, &resp));
        }
        let probability = codec::parse_gate(&resp).map_err(parse_err)?;
        Ok(GateResult { probability })
    }

    fn evaluate(state: String, questions: Vec<Question>) -> Result<Vec<Answered>, DecisionError> {
        if questions.is_empty() {
            return Err(DecisionError::InvalidRequest("no questions given".into()));
        }
        let key = ready(&state)?;
        let items: Vec<codec::QuestionItem> = questions
            .iter()
            .map(|q| codec::QuestionItem {
                id: q.id.clone(),
                instructions: q.instructions.clone(),
                kind: match &q.kind {
                    QuestionKind::Choice(options) => codec::QuestionSpec::Choice(options.clone()),
                    QuestionKind::Score(levels) => codec::QuestionSpec::Score(levels.clone()),
                    QuestionKind::Gate(c) => codec::QuestionSpec::Gate {
                        true_hint: c.true_hint.clone(),
                        false_hint: c.false_hint.clone(),
                    },
                },
            })
            .collect();
        let body = codec::evaluate_body(&model(), &state, &items);
        let (status, resp) = post_json("/v1/systemone", &key, body.as_bytes())?;
        if !(200..300).contains(&status) {
            return Err(status_error(status, &resp));
        }
        let answers = codec::parse_evaluate(&resp, &items).map_err(parse_err)?;
        Ok(answers
            .into_iter()
            .map(|(id, outcome)| Answered {
                id,
                outcome: outcome.map(|a| match a {
                    codec::AnswerSpec::Choice(p) => AnswerKind::Choice(ChoiceResult {
                        selected: p.selected,
                        confidence: p.confidence,
                        distribution: p
                            .distribution
                            .into_iter()
                            .map(|(label, score)| OptionScore { label, score })
                            .collect(),
                        flat: p.flat,
                    }),
                    codec::AnswerSpec::Score(p) => AnswerKind::Score(ScoreResult {
                        value: p.value,
                        confidence: p.confidence,
                        distribution: p
                            .distribution
                            .into_iter()
                            .map(|(label, score)| OptionScore { label, score })
                            .collect(),
                    }),
                    codec::AnswerSpec::Gate(probability) => {
                        AnswerKind::Gate(GateResult { probability })
                    }
                })
                .map_err(parse_err),
            })
            .collect())
    }

    fn describe() -> (String, bool) {
        (format!("typesafe-jev ({})", model()), true)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_splits_scheme_authority_and_path() {
        let (scheme, authority, path) = parse_url("https://api.typesafe.ai/v1/systemone").unwrap();
        assert!(matches!(scheme, Scheme::Https));
        assert_eq!(authority, "api.typesafe.ai");
        assert_eq!(path, "/v1/systemone");
    }

    #[test]
    fn parse_url_rejects_an_unknown_scheme() {
        assert!(parse_url("ftp://example.com/x").is_err());
    }

    #[test]
    fn status_error_maps_documented_statuses_to_the_right_variant() {
        assert!(matches!(status_error(422, b"bad"), DecisionError::InvalidRequest(_)));
        assert!(matches!(status_error(401, b"nope"), DecisionError::ProviderDenied(_)));
        assert!(matches!(status_error(429, b"slow down"), DecisionError::ProviderDenied(_)));
        assert!(matches!(status_error(529, b"overloaded"), DecisionError::ProviderUnavailable(_)));
        assert!(matches!(status_error(500, b"oops"), DecisionError::ProviderUnavailable(_)));
    }
}
