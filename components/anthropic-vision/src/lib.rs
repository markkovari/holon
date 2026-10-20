//! The prompt comes from the caller because the caller must parse the answer.

#[allow(warnings)]
mod bindings;

use bindings::comp::secrets::reader as secrets;
use bindings::exports::vision::describe::describer::{DescribeError, Guest};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;
use holon_llm::{anthropic, Image, Message, Part, Request, Role};

struct Component;

const DEFAULT_MODEL: &str = "claude-sonnet-5";
const DEFAULT_BASE: &str = "https://api.anthropic.com";

/// Backstop below wasmtime's 64 MiB per-store cap, past which the guest traps.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Checked here: the far end's 400 about a field is a bad place to learn a `.heic` was uploaded.
const MEDIA_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

fn model() -> String {
    config::get("vision:model")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn base_url() -> String {
    config::get("vision:base-url")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

/// Handles only what a deployment writes in config.
fn split_base(base: &str) -> (bool, String, String) {
    let (tls, rest) = match base.split_once("://") {
        Some(("http", r)) => (false, r),
        Some((_, r)) => (true, r),
        None => (true, base),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].trim_end_matches('/')),
        None => (rest, ""),
    };
    (tls, authority.to_string(), path.to_string())
}

/// The real API refuses without a key; the shim ignores auth, so it needs no secret.
fn key_required(authority: &str) -> bool {
    authority.ends_with("anthropic.com")
}

fn api_key() -> Option<String> {
    match secrets::get("anthropic-api-key") {
        Ok(Some(s)) => secrets::reveal(&s).ok().filter(|v| !v.is_empty()),
        _ => None,
    }
}

fn post(body: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let (tls, authority, prefix) = split_base(&base_url());
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let _ = headers.set("content-length", &[body.len().to_string().into_bytes()]);
    let _ = headers.set("connection", &[b"close".to_vec()]);
    let _ = headers.set("anthropic-version", &[b"2023-06-01".to_vec()]);
    if let Some(k) = api_key() {
        let _ = headers.set("x-api-key", &[k.into_bytes()]);
    }
    let req = OutgoingRequest::new(headers);
    let e = |m: &str| m.to_string();
    req.set_method(&Method::Post).map_err(|_| e("method"))?;
    req.set_scheme(Some(&if tls { Scheme::Https } else { Scheme::Http }))
        .map_err(|_| e("scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| e("authority"))?;
    req.set_path_with_query(Some(&format!("{prefix}/v1/messages"))).map_err(|_| e("path"))?;
    {
        let out = req.body().map_err(|_| e("body"))?;
        {
            let stream = out.write().map_err(|_| e("write"))?;
            for chunk in body.chunks(4096) {
                stream.blocking_write_and_flush(chunk).map_err(|_| e("body write"))?;
            }
        }
        OutgoingBody::finish(out, None).map_err(|_| e("finish"))?;
    }
    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(30_000_000_000));
    // Large photographs are slow; a shorter timeout fails on exactly the pictures worth sending.
    let _ = opts.set_first_byte_timeout(Some(180_000_000_000));
    let _ = opts.set_between_bytes_timeout(Some(180_000_000_000));

    let fut =
        outgoing_handler::handle(req, Some(opts)).map_err(|err| format!("handle: {err:?}"))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| e("no response"))?
        .map_err(|_| e("taken"))?
        .map_err(|err| format!("http: {err:?}"))?;
    let status = resp.status();

    let mut buf = Vec::new();
    if let Ok(incoming) = resp.consume() {
        if let Ok(stream) = incoming.stream() {
            loop {
                match stream.blocking_read(8192) {
                    Ok(c) if c.is_empty() => break,
                    Ok(c) => {
                        if buf.len() + c.len() > MAX_BODY_BYTES {
                            return Err(e("the response body is too large"));
                        }
                        buf.extend_from_slice(&c);
                    }
                    Err(StreamError::Closed) => break,
                    Err(_) => break,
                }
            }
        }
    }
    Ok((status, buf))
}

fn request_body(model: &str, media_type: &str, image: Vec<u8>, prompt: &str) -> String {
    let messages = [Message::new(
        Role::User,
        vec![
            Part::Image(Image { media_type: media_type.to_string(), data: image }),
            Part::Text(prompt.to_string()),
        ],
    )];
    let request = Request { max_tokens: Some(1024), ..Request::new(model, &messages) };
    anthropic::body(&request).to_string()
}

/// Joins every text block: taking only the first truncates a reply mid-JSON.
fn reply_text(bytes: &[u8]) -> String {
    anthropic::parse(bytes).map(|reply| reply.text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_request_is_the_picture_then_what_to_do_with_it() {
        let body =
            request_body("claude-x", "image/png", b"\x00\xffabc".to_vec(), "what is \"it\"?\n");
        let body: serde_json::Value = serde_json::from_str(&body).expect("the body is JSON");
        assert_eq!(
            body,
            json!({
                "model": "claude-x",
                "max_tokens": 1024,
                "messages": [{"role": "user", "content": [
                    {"type": "image", "source": {
                        "type": "base64", "media_type": "image/png", "data": "AP9hYmM="}},
                    {"type": "text", "text": "what is \"it\"?\n"}
                ]}]
            })
        );
    }

    #[test]
    fn every_text_block_is_the_answer_and_nothing_else_is() {
        let reply = json!({"content": [
            {"type": "thinking", "thinking": "hm"},
            {"type": "text", "text": "A red "},
            {"type": "text", "text": "door."},
        ]});
        assert_eq!(reply_text(reply.to_string().as_bytes()), "A red door.");
    }

    #[test]
    fn a_reply_with_no_text_is_empty_and_so_is_one_that_is_not_json() {
        assert_eq!(
            reply_text(json!({"content": [{"type": "thinking"}]}).to_string().as_bytes()),
            ""
        );
        assert_eq!(reply_text(b"not json"), "");
    }
}

impl Guest for Component {
    fn describe(
        image: Vec<u8>,
        media_type: String,
        prompt: String,
    ) -> Result<String, DescribeError> {
        if image.is_empty() {
            return Err(DescribeError::InvalidRequest("the image is empty".into()));
        }
        if image.len() > MAX_BODY_BYTES {
            return Err(DescribeError::InvalidRequest("the image is too large".into()));
        }
        let media_type = media_type.to_ascii_lowercase();
        if !MEDIA_TYPES.contains(&media_type.as_str()) {
            return Err(DescribeError::InvalidRequest(format!(
                "unsupported media type {media_type} — one of {}",
                MEDIA_TYPES.join(", ")
            )));
        }
        // Refused here, and only when a key is needed: the far end's 401 would read as a bug in this component.
        let (_, authority, _) = split_base(&base_url());
        if key_required(&authority) && api_key().is_none() {
            return Err(DescribeError::ProviderDenied(format!(
                "no anthropic-api-key in the vault, and {authority} requires one. Point \
                 `vision:base-url` at a shim to run without a key."
            )));
        }

        let body = request_body(&model(), &media_type, image, &prompt);

        let (status, bytes) = post(body.as_bytes()).map_err(DescribeError::ProviderUnavailable)?;

        let parsed: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| DescribeError::BadResponse(format!("not json: {e}")))?;

        if status != 200 {
            let detail = parsed["error"]["message"].as_str().unwrap_or("no message").to_string();
            return Err(match status {
                400 => DescribeError::InvalidRequest(detail),
                401 | 403 => DescribeError::ProviderDenied(detail),
                429 | 500..=599 => DescribeError::ProviderUnavailable(detail),
                _ => DescribeError::BadResponse(format!("{status}: {detail}")),
            });
        }

        let text = reply_text(&bytes);

        if text.trim().is_empty() {
            // A safety filter or empty reply: not a transport failure, and no retry fixes it.
            return Err(DescribeError::NoContent);
        }
        Ok(text)
    }
}

bindings::export!(Component with_types_in bindings);
