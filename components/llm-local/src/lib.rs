//! `llm-local` — send a prompt to a language model running on this machine
//!
//! Config (wasi:config/store):
//!   ollama-url      where Ollama (or an API-compatible server) listens; the app's `egress` lists it
//!   ollama-model    the model to run the prompt through, default llama3.2

#[allow(warnings)]
mod bindings;

use bindings::exports::ai::local::local::{Guest, InferError};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;

struct Component;

/// Minutes: a local model is slow, but a caller would rather hear "unavailable" than hang.
const TIMEOUT_NS: u64 = 120_000_000_000;

/// Ollama (or an API-compatible server), dialled directly: the app's `egress` lists its address.
fn ollama_url() -> Result<String, InferError> {
    match config::get("ollama-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable: nobody refused anything, the deployment never said where the model is.
        _ => Err(InferError::Unavailable(
            "ollama-url is not set — this component has nowhere to ask".into(),
        )),
    }
}

fn model() -> String {
    match config::get("ollama-model") {
        Ok(Some(m)) if !m.is_empty() => m,
        _ => "llama3.2".into(),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), InferError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(InferError::Unavailable(format!("ollama-url must be http(s), got {url:?}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), String::new()),
    };
    Ok((scheme, authority, path))
}

/// The response's status and body; every failure to get one is `unavailable`.
fn post(body: Vec<u8>) -> Result<(u16, Vec<u8>), InferError> {
    let url = ollama_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| InferError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}/api/generate"))).map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
        // Chunked: `blocking-write-and-flush` traps above 4096 bytes and a prompt is not bounded.
        for chunk in body.chunks(4096) {
            stream
                .blocking_write_and_flush(chunk)
                .map_err(|e| net(&format!("body write: {e:?}")))?;
        }
    }
    OutgoingBody::finish(out, None).map_err(|_| net("finish"))?;

    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_first_byte_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_between_bytes_timeout(Some(TIMEOUT_NS));

    let fut =
        outgoing_handler::handle(req, Some(opts)).map_err(|e| net(&format!("handle: {e:?}")))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| net("no response"))?
        .map_err(|_| net("response taken"))?
        .map_err(|e| net(&format!("http: {e:?}")))?;

    let status = resp.status();
    let body = resp.consume().map_err(|_| net("consume"))?;
    let stream = body.stream().map_err(|_| net("stream"))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(c) if c.is_empty() => break,
            Ok(c) => buf.extend_from_slice(&c),
            // Any other error would present a truncated answer as whole.
            Err(StreamError::Closed) => break,
            Err(e) => return Err(net(&format!("read: {e:?}"))),
        }
    }
    Ok((status, buf))
}

/// `stream: false`: no caller here could consume a stream.
fn request_body(model: &str, prompt: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "model": model, "prompt": prompt, "stream": false }))
        .unwrap_or_default()
}

/// Ollama's answer, or why there is none: a non-2xx reply carries `{"error": ...}`.
fn answer(status: u16, body: &[u8]) -> Result<String, InferError> {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    if !(200..300).contains(&status) {
        let why = v["error"].as_str().unwrap_or("no reason given");
        return Err(InferError::Unavailable(format!("ollama returned {status}: {why}")));
    }
    v["response"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| InferError::Unavailable("ollama reply had no response field".into()))
}

impl Guest for Component {
    fn infer(prompt: String) -> Result<String, InferError> {
        let (status, body) = post(request_body(&model(), &prompt))?;
        answer(status, &body)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_carries_the_model_and_prompt_without_streaming() {
        let v: serde_json::Value =
            serde_json::from_slice(&request_body("llama3.2", "say \"hi\"\n")).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"model": "llama3.2", "prompt": "say \"hi\"\n", "stream": false})
        );
    }

    #[test]
    fn a_response_with_quotes_and_newlines_comes_back_whole() {
        let body = br#"{"model":"llama3.2","response":"he said \"hi\"\nthen left","done":true}"#;
        assert_eq!(answer(200, body).unwrap(), "he said \"hi\"\nthen left");
    }

    #[test]
    fn a_refusal_or_an_empty_reply_is_unavailable_with_its_reason() {
        let Err(InferError::Unavailable(why)) = answer(404, br#"{"error":"model 'x' not found"}"#)
        else {
            panic!()
        };
        assert_eq!(why, "ollama returned 404: model 'x' not found");
        assert!(answer(200, br#"{"done":true}"#).is_err());
        assert!(answer(200, b"not json").is_err());
    }
}
