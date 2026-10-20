//! Raise `anthropic:timeout` when base-url is tools/claude-shim.mjs: `claude -p` has a tail past 300s.
//! The API key is a secret, never config: a token that spends money (ADR-0051).

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

const DEFAULT_BASE: &str = "https://api.anthropic.com";
// Cheapest current tier: the right default for a graph loop that makes many calls.
const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";
const DEFAULT_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;
const DEFAULT_TIMEOUT_SECS: u64 = 180;

fn cfg(key: &str) -> Option<String> {
    config::get(key).ok().flatten().filter(|s| !s.is_empty())
}

fn base_url() -> String {
    cfg("anthropic:base-url").unwrap_or_else(|| DEFAULT_BASE.to_string())
}

fn timeout_ns() -> u64 {
    cfg("anthropic:timeout")
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .saturating_mul(1_000_000_000)
}

fn default_model() -> String {
    cfg("anthropic:model").unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn api_version() -> String {
    cfg("anthropic:version").unwrap_or_else(|| DEFAULT_VERSION.to_string())
}

/// Anthropic requires a positive max_tokens, so the WIT's 0 ("no cap") is resolved here.
fn default_max_tokens() -> u32 {
    cfg("anthropic:max-tokens")
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS)
}

/// `None` is not an error: the call goes out keyless and Anthropic's 401 becomes `provider-denied`.
fn api_key() -> Option<String> {
    match secrets::get("anthropic-api-key") {
        Ok(Some(s)) => secrets::reveal(&s).ok().filter(|v| !v.is_empty()),
        _ => None,
    }
}

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

/// Retries once when the send itself fails: it never reached the server, so nothing duplicates.
/// A status answer (429, 500) is not retried; the caller decides.
fn post_json(path: &str, body: &[u8]) -> Result<(u16, Vec<u8>), InferError> {
    match post_once(path, body) {
        Err(InferError::ProviderUnavailable(first)) => match post_once(path, body) {
            Ok(ok) => Ok(ok),
            Err(InferError::ProviderUnavailable(second)) => {
                Err(InferError::ProviderUnavailable(format!("{second} (and once before: {first})")))
            }
            Err(other) => Err(other),
        },
        other => other,
    }
}

fn post_once(path: &str, body: &[u8]) -> Result<(u16, Vec<u8>), InferError> {
    let url = format!("{}{}", base_url().trim_end_matches('/'), path);
    let (scheme, authority, full_path) = parse_url(&url)?;

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    // Length-framed, not chunked: a real API edge can mishandle chunked POSTs (`IncompleteMessage`).
    let _ = headers.set("content-length", &[body.len().to_string().into_bytes()]);
    // Close, so the host is not left reading a keep-alive socket that never signals end-of-message.
    let _ = headers.set("connection", &[b"close".to_vec()]);
    let _ = headers.set("anthropic-version", &[api_version().into_bytes()]);
    if let Some(key) = api_key() {
        let _ = headers.set("x-api-key", &[key.into_bytes()]);
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
            for chunk in body.chunks(4096) {
                stream
                    .blocking_write_and_flush(chunk)
                    .map_err(|e| net(&format!("body write: {e:?}")))?;
            }
        }
        OutgoingBody::finish(out, None).map_err(|_| net("finish body"))?;
    }

    // All three set explicitly: a short unset default kills a slow call as "data receipt timed out".
    let read_ns = timeout_ns();
    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(30_000_000_000));
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

/// 429 and 529 are unavailable (the driver retries them); 401/403 are denied (terminal).
fn status_error(status: u16, body: &[u8]) -> InferError {
    let snippet = String::from_utf8_lossy(body).chars().take(300).collect::<String>();
    match status {
        400 | 422 => InferError::InvalidRequest(snippet),
        401 | 403 => InferError::ProviderDenied(format!("{status}: {snippet}")),
        429 | 529 => InferError::ProviderUnavailable(format!("{status}: {snippet}")),
        _ => InferError::ProviderUnavailable(format!("{status}: {snippet}")),
    }
}

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

impl Guest for Component {
    fn chat(messages: Vec<Message>, opts: Options) -> Result<Completion, InferError> {
        if messages.is_empty() {
            return Err(InferError::InvalidRequest("no messages".into()));
        }
        let model = if opts.model.is_empty() { default_model() } else { opts.model.clone() };
        let max_tokens = if opts.max_tokens > 0 { opts.max_tokens } else { default_max_tokens() };

        let msgs: Vec<codec::Msg> = messages
            .iter()
            .map(|m| codec::Msg { role: role_str(m.role), content: &m.content })
            .collect();
        let copts = codec::Opts {
            model: &model,
            temperature: opts.temperature,
            max_tokens,
            stop: opts.stop.clone(),
        };
        let body = codec::messages_body(&msgs, &copts);
        let (status, resp) = post_json("/v1/messages", body.as_bytes())?;
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

    fn embed(_text: String, _opts: Options) -> Result<Vec<f32>, InferError> {
        // Anthropic has no embeddings endpoint; refused rather than faked.
        Err(InferError::InvalidRequest(
            "anthropic has no embeddings endpoint — link a dedicated embedding provider".into(),
        ))
    }

    fn describe() -> (String, bool) {
        (default_model(), false)
    }
}

bindings::export!(Component with_types_in bindings);
