//! `llm-queue` — the free LLM queue (ADR-0126): a task in, the first free provider's answer out.
//!
//! `POST /ask` takes `{"id", "prompt", "system_prompt"?, "max_tokens"?}` and answers
//! `{"id", "text", "ok", "error"?}`, always 200: the reply is the body. comp-host delivers the
//! `holon.llm.ask` requests here (`--subscribe holon.llm.ask=/ask`, ADR-0131).
//!
//! The providers (`providers.json`) are asked in order; each one's models are discovered, and each
//! model's documented limits and the cooldowns its refusals named are kept in keyvalue, since an
//! instance lives for one request. A provider's key is the `<name>-api-key` secret, else config.
//!
//! Config (wasi:config/store):
//!   llm-providers   the provider list as JSON, in `providers.json`'s shape; the built-in one when unset

#[allow(warnings)]
mod bindings;
mod providers;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::comp::secrets::reader as secrets;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::config::store as config;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingRequest, OutgoingResponse,
    RequestOptions, ResponseOutparam, Scheme,
};
use bindings::wasi::keyvalue::store;
use providers::{Health, Kind, Provider, Stored};

struct Component;

/// An agent's transcript fits easily; a model's answer is far smaller.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// How many of one provider's models a request may try before moving on.
const MODELS_PER_PROVIDER: usize = 3;
/// A provider's model list is asked again after this long.
const MODELS_TTL_MS: u64 = 6 * 3600 * 1000;

guestio::guest_write_all!();
guestio::guest_read_body!(MAX_BODY_BYTES);

#[derive(Deserialize)]
struct Task {
    #[serde(default)]
    id: String,
    prompt: String,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    max_tokens: Option<u32>,
}

fn cfg(key: &str) -> Option<String> {
    config::get(key).ok().flatten().filter(|s| !s.is_empty())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn secs_until(t: SystemTime, now: SystemTime) -> u64 {
    t.duration_since(now).unwrap_or_default().as_secs()
}

fn api_key(provider: &str) -> Option<String> {
    let name = format!("{provider}-api-key");
    match secrets::get(&name) {
        Ok(Some(s)) => secrets::reveal(&s).ok().filter(|v| !v.is_empty()),
        _ => None,
    }
    .or_else(|| cfg(&name))
}

fn bucket() -> Option<store::Bucket> {
    store::open("default").ok()
}

fn load_health(key: &str) -> Health {
    bucket()
        .and_then(|b| b.get(&format!("llmq:health:{key}")).ok().flatten())
        .and_then(|v| serde_json::from_slice::<Stored>(&v).ok())
        .unwrap_or_default()
        .into()
}

fn save_health(key: &str, h: &Health) {
    if let (Some(b), Ok(v)) = (bucket(), serde_json::to_vec(&Stored::from(h))) {
        let _ = b.set(&format!("llmq:health:{key}"), &v);
    }
}

/// Load, change, store: two requests at once can each count one request the other misses.
fn with_health<R>(key: &str, f: impl FnOnce(&mut Health) -> R) -> R {
    let mut h = load_health(key);
    let r = f(&mut h);
    save_health(key, &h);
    r
}

/// A provider's refusal, as it said it.
struct Failure {
    status: Option<u16>,
    retry_after: Option<String>,
    body: String,
}

fn fetch(
    method: Method,
    url: &str,
    headers: &[(&str, String)],
    body: Option<Vec<u8>>,
) -> Result<(u16, Option<String>, Vec<u8>), String> {
    let (scheme, rest) = match url.split_once("://") {
        Some(("https", r)) => (Scheme::Https, r),
        Some(("http", r)) => (Scheme::Http, r),
        _ => return Err(format!("not an http(s) url: {url}")),
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let fields = Fields::new();
    for (k, v) in headers {
        let _ = fields.set(k, &[v.as_bytes().to_vec()]);
    }
    if body.is_some() {
        let _ = fields.set("content-type", &[b"application/json".to_vec()]);
    }
    let req = OutgoingRequest::new(fields);
    let e = |m: &str| m.to_string();
    req.set_method(&method).map_err(|_| e("method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| e("scheme"))?;
    req.set_authority(Some(authority)).map_err(|_| e("authority"))?;
    req.set_path_with_query(Some(if path.is_empty() { "/" } else { path }))
        .map_err(|_| e("path"))?;
    let out = req.body().map_err(|_| e("body"))?;
    if let Some(bytes) = &body {
        let stream = out.write().map_err(|_| e("body stream"))?;
        if !write_all(&stream, bytes) {
            return Err(e("writing the body"));
        }
    }
    OutgoingBody::finish(out, None).map_err(|_| e("finishing the body"))?;
    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(15_000_000_000));
    // Thinking models take over a minute on an agent's long transcript.
    let _ = opts.set_first_byte_timeout(Some(160_000_000_000));
    let fut = bindings::wasi::http::outgoing_handler::handle(req, Some(opts))
        .map_err(|err| format!("{err:?}"))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| e("no response"))?
        .map_err(|_| e("response taken"))?
        .map_err(|err| format!("{err:?}"))?;
    let status = resp.status();
    let retry_after =
        resp.headers().get("retry-after").first().map(|v| String::from_utf8_lossy(v).into_owned());
    let incoming = resp.consume().map_err(|_| e("response body"))?;
    let stream = incoming.stream().map_err(|_| e("response stream"))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(c) if c.is_empty() => break,
            Ok(c) => {
                buf.extend_from_slice(&c);
                if buf.len() > MAX_BODY_BYTES {
                    return Err(format!("answered more than {MAX_BODY_BYTES} bytes"));
                }
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // A failed read is not the end: a truncated reply would parse into a wrong whole one.
            Err(err) => return Err(format!("reading: {err:?}")),
        }
    }
    Ok((status, retry_after, buf))
}

fn auth(p: &Provider, key: &str) -> (&'static str, String) {
    match p.kind {
        Kind::Gemini => ("x-goog-api-key", key.to_string()),
        Kind::Openai => ("authorization", format!("Bearer {key}")),
    }
}

/// The provider's offered models worth asking, best first, listed once and kept a while.
fn models_for(p: &Provider, key: &str) -> Result<Vec<String>, String> {
    let cache = format!("llmq:models:{}", p.name);
    if let Some(v) = bucket().and_then(|b| b.get(&cache).ok().flatten()) {
        if let Ok((at, models)) = serde_json::from_slice::<(u64, Vec<String>)>(&v) {
            if now_ms().saturating_sub(at) < MODELS_TTL_MS && !models.is_empty() {
                return Ok(models);
            }
        }
    }
    let url =
        format!("{}/models{}", p.base, if p.kind == Kind::Gemini { "?pageSize=1000" } else { "" });
    let (status, _, body) = fetch(Method::Get, &url, &[auth(p, key)], None)?;
    if !(200..300).contains(&status) {
        let now = SystemTime::now();
        let (back, why) =
            providers::back_at(p, Some(status), None, &String::from_utf8_lossy(&body), now);
        let why = format!("cannot list its models ({why})");
        with_health(&p.name, |h| {
            h.until = Some(back.max(now + Duration::from_secs(60)));
            h.why = why.clone();
        });
        return Err(why);
    }
    let v: Value = serde_json::from_slice(&body).unwrap_or_default();
    let offered: Vec<String> = match p.kind {
        Kind::Gemini => v["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["supportedGenerationMethods"].to_string().contains("generateContent"))
            .filter_map(|m| m["name"].as_str().map(|n| n.trim_start_matches("models/").to_string()))
            .collect(),
        Kind::Openai => v["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["id"].as_str().map(String::from))
            .collect(),
    };
    let wanted = providers::candidates(&p.models, &offered);
    if wanted.is_empty() {
        let why = format!("offers none of its wanted models; it offers {}", offered.join(", "));
        with_health(&p.name, |h| {
            h.until = Some(SystemTime::now() + Duration::from_secs(3600));
            h.why = why.clone();
        });
        return Err(why);
    }
    if let (Some(b), Ok(v)) = (bucket(), serde_json::to_vec(&(now_ms(), &wanted))) {
        let _ = b.set(&cache, &v);
    }
    Ok(wanted)
}

fn ask(p: &Provider, model: &str, key: &str, task: &Task) -> Result<String, Failure> {
    let messages = [holon_llm::Message {
        role: holon_llm::Role::User,
        parts: vec![holon_llm::Part::Text(task.prompt.clone())],
    }];
    let system = task.system_prompt.as_deref().filter(|s| !s.is_empty());
    let req = holon_llm::Request {
        model,
        messages: &messages,
        system,
        tools: &[],
        max_tokens: Some(task.max_tokens.unwrap_or(8192)),
        temperature: Some(0.1),
        stop: &[],
        seed: None,
        stream: false,
        cache: false,
        blocks: false,
    };
    let (url, body) = match p.kind {
        Kind::Gemini => {
            (format!("{}/models/{model}:generateContent", p.base), holon_llm::gemini::body(&req))
        }
        Kind::Openai => (format!("{}/chat/completions", p.base), holon_llm::openai::body(&req)),
    };
    let fail = |status, retry_after, body| Failure { status, retry_after, body };
    let (status, retry_after, bytes) =
        fetch(Method::Post, &url, &[auth(p, key)], Some(body.to_string().into_bytes()))
            .map_err(|e| fail(None, None, e))?;
    if !(200..300).contains(&status) {
        return Err(fail(Some(status), retry_after, String::from_utf8_lossy(&bytes).into_owned()));
    }
    let reply = match p.kind {
        Kind::Gemini => holon_llm::gemini::reply(&bytes).map_err(|e| format!("{e:?}")),
        Kind::Openai => holon_llm::openai::parse(&bytes).map_err(|e| format!("{e:?}")),
    }
    .map_err(|e| fail(Some(status), None, e))?;
    if reply.text.trim().is_empty() {
        let why = format!(
            "answered no text (finish_reason {}, {} tool call(s))",
            reply.finish.as_deref().unwrap_or("none"),
            reply.calls.len()
        );
        return Err(fail(Some(status), None, why));
    }
    Ok(reply.text)
}

/// The free providers' models in order, skipping any cooling down, at its documented limits or
/// without a key. A failure sets when that model (or, for a refused key, the provider) may be asked
/// again, and the next is tried at once.
fn answer(task: &Task, providers: &[Provider]) -> Result<String, String> {
    let mut passed = Vec::new();
    for p in providers {
        let now = SystemTime::now();
        let unlimited = Provider { rpm: 0, rpd: 0, ..p.clone() };
        if let Some((back, why)) = with_health(&p.name, |h| h.blocked(&unlimited, now)) {
            passed.push(format!("{} {why}, back in {}s", p.name, secs_until(back, now)));
            continue;
        }
        let Some(key) = api_key(&p.name) else {
            passed.push(format!("{} has no key", p.name));
            continue;
        };
        let models = match models_for(p, &key) {
            Ok(m) => m,
            Err(why) => {
                passed.push(format!("{} {why}", p.name));
                continue;
            }
        };
        for model in models.iter().take(MODELS_PER_PROVIDER) {
            let slot = format!("{}/{model}", p.name);
            let now = SystemTime::now();
            let blocked = with_health(&slot, |h| {
                let b = h.blocked(p, now);
                if b.is_none() {
                    h.spent(p, now);
                }
                b
            });
            if let Some((back, why)) = blocked {
                passed.push(format!("{slot} {why}, back in {}s", secs_until(back, now)));
                continue;
            }
            let f = match ask(p, model, &key, task) {
                Ok(text) => return Ok(text),
                Err(f) => f,
            };
            if let Some(call) = providers::salvaged_tool_call(&f.body) {
                return Ok(call);
            }
            let now = SystemTime::now();
            let (back, why) =
                providers::back_at(p, f.status, f.retry_after.as_deref(), &f.body, now);
            let detail: String = f.body.chars().take(200).collect();
            eprintln!("llm-queue: {slot} {why}, back in {}s: {detail}", secs_until(back, now));
            passed.push(format!("{slot} {why}"));
            let whole = matches!(f.status, Some(401 | 403));
            with_health(if whole { &p.name } else { &slot }, |h| {
                h.until = Some(back);
                h.why = why;
            });
            if f.status == Some(404) {
                if let Some(b) = bucket() {
                    let _ = b.delete(&format!("llmq:models:{}", p.name));
                }
            }
            if whole {
                break;
            }
        }
    }
    Err(format!("no free provider answered: {}", passed.join("; ")))
}

fn providers() -> Result<Vec<Provider>, String> {
    match cfg("llm-providers") {
        Some(json) => serde_json::from_str(&json).map_err(|e| format!("llm-providers: {e}")),
        None => Ok(providers::defaults()),
    }
}

fn respond(out: ResponseOutparam, status: u16, body: &[u8]) {
    let fields = Fields::new();
    let _ = fields.set("content-type", &[b"application/json".to_vec()]);
    let resp = OutgoingResponse::new(fields);
    let _ = resp.set_status_code(status);
    let Ok(b) = resp.body() else { return };
    ResponseOutparam::set(out, Ok(resp));
    if let Ok(stream) = b.write() {
        let _ = write_all(&stream, body);
    }
    let _ = OutgoingBody::finish(b, None);
}

/// The reply's shape is holon-llmbatch's, so a caller of either reads both.
fn reply(id: &str, result: Result<String, String>) -> Vec<u8> {
    match result {
        Ok(text) => json!({"id": id, "text": text, "ok": true}),
        Err(error) => json!({"id": id, "text": "", "ok": false, "error": error}),
    }
    .to_string()
    .into_bytes()
}

impl Guest for Component {
    fn handle(request: IncomingRequest, out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_default();
        match (request.method(), path.split('?').next().unwrap_or("")) {
            (Method::Post, "/ask") => {
                let Ok(body) = read_body(&request) else {
                    return respond(
                        out,
                        413,
                        &reply("", Err(format!("a task is at most {MAX_BODY_BYTES} bytes"))),
                    );
                };
                let result = serde_json::from_slice::<Task>(&body)
                    .map_err(|e| format!("not a task: {e}"))
                    .and_then(|task| {
                        providers().and_then(|ps| answer(&task, &ps)).map(|t| (task.id, t))
                    });
                match result {
                    Ok((id, text)) => respond(out, 200, &reply(&id, Ok(text))),
                    Err(e) => {
                        let id = serde_json::from_slice::<Value>(&body)
                            .ok()
                            .and_then(|v| v["id"].as_str().map(String::from))
                            .unwrap_or_default();
                        respond(out, 200, &reply(&id, Err(e)))
                    }
                }
            }
            _ => respond(out, 404, br#"{"error":"POST /ask"}"#),
        }
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_reads_like_holon_llmbatch() {
        let ok: Value = serde_json::from_slice(&reply("t1", Ok("hi".into()))).unwrap();
        assert_eq!(ok, json!({"id": "t1", "text": "hi", "ok": true}));
        let err: Value = serde_json::from_slice(&reply("t2", Err("no".into()))).unwrap();
        assert_eq!((err["ok"].as_bool(), err["error"].as_str()), (Some(false), Some("no")));
    }

    #[test]
    fn a_task_needs_only_a_prompt() {
        let t: Task = serde_json::from_str(r#"{"prompt": "p", "reply_subject": null}"#).unwrap();
        assert_eq!((t.prompt.as_str(), t.id.as_str(), t.max_tokens), ("p", "", None));
    }
}
