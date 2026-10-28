//! `llm-router` — Smart cascading LLM router with free-tier pool bubbling and circuit breaking.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

pub mod codec;
pub mod router;

#[cfg_attr(not(target_arch = "wasm32"), allow(unused_imports))]
use router::{CircuitBreaker, ProviderConfig, RouterPool, TokenBucket};

#[cfg(target_arch = "wasm32")]
use std::sync::Mutex;

#[cfg(target_arch = "wasm32")]
use bindings::comp::secrets::reader as secrets;
#[cfg(target_arch = "wasm32")]
use bindings::exports::llm::inference::inference::{
    Completion, Guest, InferError, Message, Options, Role, Usage,
};
#[cfg(target_arch = "wasm32")]
use bindings::wasi::clocks::monotonic_clock;
#[cfg(target_arch = "wasm32")]
use bindings::wasi::config::store as config;
#[cfg(target_arch = "wasm32")]
use bindings::wasi::http::outgoing_handler;
#[cfg(target_arch = "wasm32")]
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
#[cfg(target_arch = "wasm32")]
use bindings::wasi::io::streams::StreamError;

pub struct Component;

#[cfg(target_arch = "wasm32")]
static POOL: Mutex<Option<RouterPool>> = Mutex::new(None);

#[cfg(target_arch = "wasm32")]
fn now_nanos() -> u64 {
    monotonic_clock::now()
}

#[cfg(target_arch = "wasm32")]
fn cfg(key: &str) -> Option<String> {
    config::get(key).ok().flatten().filter(|s| !s.is_empty())
}

#[cfg(target_arch = "wasm32")]
fn secret_or_cfg(secret_name: &str, cfg_key: &str) -> Option<String> {
    match secrets::get(secret_name) {
        Ok(Some(s)) => secrets::reveal(&s).ok().filter(|v| !v.is_empty()),
        _ => cfg(cfg_key),
    }
}

pub fn default_providers(now_ns: u64) -> Vec<ProviderConfig> {
    vec![
        // Tier 1: Free Tier Pool
        ProviderConfig {
            id: "gemini".into(),
            tier: 1,
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".into(),
            default_model: "gemini-2.5-flash".into(),
            secret_key_name: Some("gemini-api-key".into()),
            bucket: TokenBucket::new(15, now_ns),
            breaker: CircuitBreaker::new(2, 60),
        },
        ProviderConfig {
            id: "groq".into(),
            tier: 1,
            base_url: "https://api.groq.com/openai/v1".into(),
            default_model: "llama-3.3-70b-versatile".into(),
            secret_key_name: Some("groq-api-key".into()),
            bucket: TokenBucket::new(30, now_ns),
            breaker: CircuitBreaker::new(2, 60),
        },
        ProviderConfig {
            id: "cerebras".into(),
            tier: 1,
            base_url: "https://api.cerebras.ai/v1".into(),
            default_model: "llama3.1-8b".into(),
            secret_key_name: Some("cerebras-api-key".into()),
            bucket: TokenBucket::new(30, now_ns),
            breaker: CircuitBreaker::new(2, 60),
        },
        // Tier 2: Sub-Cent Paid
        ProviderConfig {
            id: "deepseek".into(),
            tier: 2,
            base_url: "https://api.deepseek.com/v1".into(),
            default_model: "deepseek-chat".into(),
            secret_key_name: Some("deepseek-api-key".into()),
            bucket: TokenBucket::unmetered(),
            breaker: CircuitBreaker::new(3, 60),
        },
        // Tier 3: Frontier Fallback
        ProviderConfig {
            id: "openai".into(),
            tier: 3,
            base_url: "https://api.openai.com/v1".into(),
            default_model: "gpt-4o-mini".into(),
            secret_key_name: Some("openai-api-key".into()),
            bucket: TokenBucket::unmetered(),
            breaker: CircuitBreaker::new(3, 60),
        },
    ]
}

#[cfg(target_arch = "wasm32")]
fn with_pool<R>(f: impl FnOnce(&mut RouterPool) -> R) -> R {
    let mut guard = POOL.lock().unwrap();
    if guard.is_none() {
        let now = now_nanos();
        let mut providers = default_providers(now);
        // Overlay runtime config overrides if set
        if let Some(url) = cfg("router:gemini:base") {
            providers[0].base_url = url;
        }
        if let Some(m) = cfg("router:gemini:model") {
            providers[0].default_model = m;
        }
        if let Some(rpm) = cfg("router:gemini:rpm").and_then(|s| s.parse().ok()) {
            providers[0].bucket.rpm = rpm;
        }
        if let Some(url) = cfg("router:groq:base") {
            providers[1].base_url = url;
        }
        if let Some(m) = cfg("router:groq:model") {
            providers[1].default_model = m;
        }
        if let Some(rpm) = cfg("router:groq:rpm").and_then(|s| s.parse().ok()) {
            providers[1].bucket.rpm = rpm;
        }
        *guard = Some(RouterPool::new(providers));
    }
    f(guard.as_mut().unwrap())
}

#[cfg(target_arch = "wasm32")]
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

#[cfg(target_arch = "wasm32")]
fn post_json(
    base: &str,
    path: &str,
    api_key: Option<&str>,
    body: &[u8],
) -> Result<(u16, Vec<u8>), InferError> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let (scheme, authority, full_path) = parse_url(&url)?;

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    if let Some(key) = api_key {
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
            for chunk in body.chunks(4096) {
                stream
                    .blocking_write_and_flush(chunk)
                    .map_err(|e| net(&format!("body write: {e:?}")))?;
            }
        }
        OutgoingBody::finish(out, None).map_err(|_| net("finish body"))?;
    }

    let opts = RequestOptions::new();
    let timeout_ns = 30_000_000_000; // 30 seconds
    let _ = opts.set_connect_timeout(Some(10_000_000_000));
    let _ = opts.set_first_byte_timeout(Some(timeout_ns));
    let _ = opts.set_between_bytes_timeout(Some(timeout_ns));

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

#[cfg(target_arch = "wasm32")]
fn role_str(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

#[cfg(target_arch = "wasm32")]
fn parse_err(e: codec::ParseError) -> InferError {
    match e {
        codec::ParseError::BadResponse(m) => InferError::BadResponse(m),
        codec::ParseError::NoContent => InferError::NoContent,
    }
}

#[cfg(target_arch = "wasm32")]
impl Guest for Component {
    fn chat(messages: Vec<Message>, opts: Options) -> Result<Completion, InferError> {
        let msgs: Vec<codec::Msg> = messages
            .iter()
            .map(|m| codec::Msg {
                role: role_str(m.role),
                content: &m.content,
            })
            .collect();

        let now = now_nanos();
        let plan = with_pool(|pool| pool.plan_invocation(now));
        if plan.is_empty() {
            return Err(InferError::ProviderUnavailable(
                "all router pools are exhausted or rate-limited".into(),
            ));
        }

        let mut last_err = InferError::ProviderUnavailable("no providers attempted".into());

        for provider_idx in plan {
            let (provider_id, base_url, model, key) = with_pool(|pool| {
                let p = &pool.providers[provider_idx];
                let key = p
                    .secret_key_name
                    .as_deref()
                    .and_then(|s| secret_or_cfg(s, &format!("router:{}:key", p.id)));
                (p.id.clone(), p.base_url.clone(), p.default_model.clone(), key)
            });

            let chosen_model = if opts.model.is_empty() {
                model.as_str()
            } else {
                opts.model.as_str()
            };

            let c_opts = codec::Opts {
                model: chosen_model,
                temperature: opts.temperature,
                max_tokens: opts.max_tokens,
                stop: opts.stop.clone(),
                seed: opts.seed,
            };

            let body = codec::chat_body(&msgs, &c_opts);
            match post_json(&base_url, "/chat/completions", key.as_deref(), body.as_bytes()) {
                Ok((200, resp_bytes)) => {
                    match codec::parse_completion(&resp_bytes) {
                        Ok(parsed) => {
                            with_pool(|pool| {
                                pool.providers[provider_idx].breaker.record_success();
                            });
                            return Ok(Completion {
                                text: parsed.text,
                                finish_reason: parsed.finish_reason,
                                model: format!("{}:{}", provider_id, parsed.model),
                                usage: Usage {
                                    prompt_tokens: parsed.prompt_tokens,
                                    completion_tokens: parsed.completion_tokens,
                                },
                            });
                        }
                        Err(e) => {
                            last_err = parse_err(e);
                        }
                    }
                }
                Ok((status, resp_bytes)) => {
                    let now_fail = now_nanos();
                    let snippet = String::from_utf8_lossy(&resp_bytes)
                        .chars()
                        .take(200)
                        .collect::<String>();
                    if status == 429 || status >= 500 {
                        with_pool(|pool| {
                            pool.providers[provider_idx].breaker.record_failure(now_fail);
                        });
                    }
                    last_err =
                        InferError::ProviderDenied(format!("{provider_id} {status}: {snippet}"));
                }
                Err(e) => {
                    let now_fail = now_nanos();
                    with_pool(|pool| {
                        pool.providers[provider_idx].breaker.record_failure(now_fail);
                    });
                    last_err = e;
                }
            }
        }

        Err(last_err)
    }

    fn complete(prompt: String, system: String, opts: Options) -> Result<Completion, InferError> {
        let mut msgs = Vec::new();
        if !system.is_empty() {
            msgs.push(Message {
                role: Role::System,
                content: system,
            });
        }
        msgs.push(Message {
            role: Role::User,
            content: prompt,
        });
        Self::chat(msgs, opts)
    }

    fn embed(text: String, opts: Options) -> Result<Vec<f32>, InferError> {
        let now = now_nanos();
        let plan = with_pool(|pool| pool.plan_invocation(now));
        let provider_idx = plan.first().copied().unwrap_or(0);

        let (base_url, key) = with_pool(|pool| {
            let p = &pool.providers[provider_idx];
            let key = p
                .secret_key_name
                .as_deref()
                .and_then(|s| secret_or_cfg(s, &format!("router:{}:key", p.id)));
            (p.base_url.clone(), key)
        });

        let model = if opts.model.is_empty() {
            "text-embedding-3-small"
        } else {
            &opts.model
        };

        let body = codec::embed_body(&text, model);
        let (status, resp_bytes) =
            post_json(&base_url, "/embeddings", key.as_deref(), body.as_bytes())?;
        if status != 200 {
            return Err(InferError::ProviderUnavailable(format!("embed status: {status}")));
        }
        codec::parse_embedding(&resp_bytes).map_err(parse_err)
    }

    fn describe() -> (String, bool) {
        ("smart-cascading-router".to_string(), true)
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
