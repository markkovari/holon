//! `openai-provider` — the OpenAI side of the LLM contract — chat, completion and embeddings over an OpenAI-compatible API
//! OpenAI-compatible HTTP API.
//!
//! Implements the vendor-agnostic `llm:inference/inference` by POSTing to
//! `/v1/chat/completions` and `/v1/embeddings`. Works against OpenAI, Azure
//! OpenAI, Together, Groq, vLLM, or a local Ollama/llama.cpp server — anything
//! that speaks the OpenAI JSON contract. The endpoint and models come from
//! wasi:config; nothing about the vendor is in the WIT.
//!
//! HTTP idiom (build OutgoingRequest -> write JSON body -> handle -> block ->
//! read full response body) mirrors notify-dispatch's `post`, extended to
//! return the response BODY (the model's answer) and to set a bearer header.
//!
//! Config (wasi:config/store):
//!   openai:base-url     default "https://api.openai.com/v1"
//!   openai:model        default chat model (default "gpt-4o-mini")
//!   openai:embed-model  default embedding model (default
//!                       "text-embedding-3-small")
//!   openai:timeout      seconds to wait for the first response byte, and
//!                       between bytes after it (default 600)
//!   openai:max-tokens   the cap to send when the caller sets none (default
//!                       4096)
//!
//! Secret (comp:secrets/reader):
//!   openai-api-key      bearer token, granted by reference in the manifest
//!
//! The key is a SECRET and not config, which is the one thing about this
//! component that is not a deployment preference. A config map is readable by
//! anything that can read the deployment and is dumped by every debug path
//! there is (ADR-0051); this token spends money on someone else's account. A
//! local server that ignores auth simply grants nothing and no header is sent.

#[allow(warnings)]
mod bindings;
mod codec;

use bindings::comp::secrets::reader as secrets;
use bindings::exports::llm::inference::inference::{
    Completion, Guest, InferError, Message, Options, Role, Usage,
};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;

struct Component;

const DEFAULT_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-4o-mini";
const DEFAULT_EMBED_MODEL: &str = "text-embedding-3-small";
/// How long to wait for a token, when nobody says.
///
/// Ten minutes, which is far past anything a hosted API does and exactly right
/// for the case this exists to serve: a self-hosted server generating on one
/// machine. Benchmarked on an M2 Max serving Qwen3-Coder-30B-A3B-4bit
/// (`mlx_lm.benchmark`, single stream): decode runs at 72 tok/s on a 1k prompt,
/// 47 at 8k and 34 at 16k, while prefill holds ~350 tok/s. So a 12000-token
/// answer at a realistic context length is roughly six minutes, and a budget
/// picked for a cloud endpoint kills it mid-sentence. The caller reads that as
/// `provider-unavailable`, which is a server that was working fine and a
/// stopwatch that was not.
///
/// Decode falls with context because attention reads the whole KV cache per
/// token, so the number to size against is the one at the context you actually
/// send, not the headline.
const DEFAULT_TIMEOUT_SECS: u64 = 600;
/// The `max_tokens` to send when the caller asked for none.
///
/// The codec OMITS the field at zero, which is not the same as unlimited: the
/// server then applies its own default, and mlx_lm's is small enough to cut a
/// generated file in half. `agent-writer` deliberately sends 0 and expects the
/// deployment to decide the budget, so this is where that decision lives.
const DEFAULT_MAX_TOKENS: u32 = 4096;

// ---- config -------------------------------------------------------------

fn cfg(key: &str) -> Option<String> {
    config::get(key).ok().flatten().filter(|s| !s.is_empty())
}

fn base_url() -> String {
    cfg("openai:base-url").unwrap_or_else(|| DEFAULT_BASE.to_string())
}

fn default_model() -> String {
    cfg("openai:model").unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// The read budget, in nanoseconds, from `openai:timeout`.
///
/// A zero or unparseable value falls back rather than disabling the timeout: a
/// misconfigured budget that hangs forever is worse than one that is wrong.
fn default_max_tokens() -> u32 {
    cfg("openai:max-tokens")
        .and_then(|s| s.trim().parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS)
}

fn timeout_ns() -> u64 {
    cfg("openai:timeout")
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .saturating_mul(1_000_000_000)
}

fn default_embed_model() -> String {
    cfg("openai:embed-model").unwrap_or_else(|| DEFAULT_EMBED_MODEL.to_string())
}

/// The bearer token, from the vault.
///
/// `none` is not an error here: a local Ollama or llama.cpp ignores auth
/// entirely, and refusing to run without a key would make the offline case —
/// the one a graph loop iterates against — impossible. A real provider answers
/// 401 and that becomes `provider-denied`, which says what happened far better
/// than a guess made before the call.
fn api_key() -> Option<String> {
    match secrets::get("openai-api-key") {
        Ok(Some(s)) => secrets::reveal(&s).ok().filter(|v| !v.is_empty()),
        _ => None,
    }
}

// ---- http ---------------------------------------------------------------

fn parse_url(url: &str) -> Result<(Scheme, String, String), InferError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(InferError::ProviderUnavailable(format!("bad url scheme: {url}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), "/".to_string()),
    };
    Ok((scheme, authority, path))
}

/// POST `body` as application/json to `base_url + path` with an optional bearer
/// token. Returns (status, response-body-bytes). Network failures map to
/// `provider-unavailable`.
fn post_json(path: &str, body: &[u8]) -> Result<(u16, Vec<u8>), InferError> {
    let url = format!("{}{}", base_url().trim_end_matches('/'), path);
    let (scheme, authority, full_path) = parse_url(&url)?;

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    if let Some(key) = api_key() {
        let _ = headers.set("authorization", &[format!("Bearer {key}").into_bytes()]);
    }

    let req = OutgoingRequest::new(headers);
    let net = |m: &str| InferError::ProviderUnavailable(m.to_string());
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

    // Unset before this, which meant whatever the host defaults to — and a host
    // default is picked for an API that answers in seconds, not for a local model
    // that spends twenty minutes decoding. Connect stays short because a refused
    // connection is immediate; the READ budget is the one that has to be generous.
    let opts = RequestOptions::new();
    let read_ns = timeout_ns();
    let _ = opts.set_connect_timeout(Some(30_000_000_000)); // 30s
    let _ = opts.set_first_byte_timeout(Some(read_ns));
    let _ = opts.set_between_bytes_timeout(Some(read_ns));

    let future = outgoing_handler::handle(req, Some(opts))
        .map_err(|e| InferError::ProviderUnavailable(format!("http handle: {e:?}")))?;
    future.subscribe().block();
    let resp = future
        .get()
        .ok_or_else(|| net("no response"))?
        .map_err(|_| net("response taken"))?
        .map_err(|e| InferError::ProviderUnavailable(format!("http: {e:?}")))?;

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

/// Map a non-2xx status to the right infer-error, carrying the body snippet.
fn status_error(status: u16, body: &[u8]) -> InferError {
    let snippet = String::from_utf8_lossy(body).chars().take(300).collect::<String>();
    match status {
        400 | 422 => InferError::InvalidRequest(snippet),
        401 | 403 | 429 => InferError::ProviderDenied(format!("{status}: {snippet}")),
        _ => InferError::ProviderUnavailable(format!("{status}: {snippet}")),
    }
}

// ---- bindings <-> codec glue --------------------------------------------
// The request shaping + response parsing live in `codec` (host-testable, no
// WASI). Here we resolve config defaults, convert the WIT records to codec
// types, and map codec errors back to `infer-error`.

fn role_str(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn parse_err(e: codec::ParseError) -> InferError {
    match e {
        codec::ParseError::BadResponse(m) => InferError::BadResponse(m),
        codec::ParseError::NoContent => InferError::NoContent,
    }
}

// ---- guest --------------------------------------------------------------

impl Guest for Component {
    fn chat(messages: Vec<Message>, opts: Options) -> Result<Completion, InferError> {
        if messages.is_empty() {
            return Err(InferError::InvalidRequest("no messages".into()));
        }
        let model = if opts.model.is_empty() { default_model() } else { opts.model.clone() };
        let msgs: Vec<codec::Msg> = messages
            .iter()
            .map(|m| codec::Msg { role: role_str(m.role), content: &m.content })
            .collect();
        let copts = codec::Opts {
            model: &model,
            temperature: opts.temperature,
            max_tokens: if opts.max_tokens > 0 { opts.max_tokens } else { default_max_tokens() },
            stop: opts.stop.clone(),
            seed: opts.seed,
        };
        let body = codec::chat_body(&msgs, &copts);
        let (status, resp) = post_json("/chat/completions", body.as_bytes())?;
        if !(200..300).contains(&status) {
            return Err(status_error(status, &resp));
        }
        let p = codec::parse_completion(&resp).map_err(parse_err)?;
        Ok(Completion {
            text: p.text,
            finish_reason: p.finish_reason,
            model: p.model,
            usage: Usage { prompt_tokens: p.prompt_tokens, completion_tokens: p.completion_tokens },
        })
    }

    fn complete(prompt: String, system: String, opts: Options) -> Result<Completion, InferError> {
        let mut messages = Vec::new();
        if !system.is_empty() {
            messages.push(Message { role: Role::System, content: system });
        }
        messages.push(Message { role: Role::User, content: prompt });
        Self::chat(messages, opts)
    }

    fn embed(text: String, opts: Options) -> Result<Vec<f32>, InferError> {
        if text.is_empty() {
            return Err(InferError::InvalidRequest("empty text".into()));
        }
        let model = if opts.model.is_empty() { default_embed_model() } else { opts.model.clone() };
        let body = codec::embed_body(&text, &model);
        let (status, resp) = post_json("/embeddings", body.as_bytes())?;
        if !(200..300).contains(&status) {
            return Err(status_error(status, &resp));
        }
        codec::parse_embedding(&resp).map_err(parse_err)
    }

    fn describe() -> (String, bool) {
        // default model + embeddings available (this provider implements embed).
        (default_model(), true)
    }
}

bindings::export!(Component with_types_in bindings);
