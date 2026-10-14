//! `deepseek-vision` — show a model a picture and get back what it says, over DeepSeek's
//! OpenAI-compatible Chat Completions API.
//!
//! The provider half of `vision:describe`, beside `anthropic-vision`. A guest hands over image
//! bytes and a prompt and never learns which vendor looked. Same shape as that provider: egress
//! to exactly one authority, the key REVEALED from the vault rather than read out of config, and
//! the model and base URL chosen at deploy time.
//!
//! ## Why this is a component and not a script
//!
//! ADR-0095: a thing is native only when WASI cannot express it, and a vision call is an HTTPS
//! POST. So the request, the reply and the failure mapping are ordinary Rust and unit-tested
//! below on the host; only the socket is a guest import.
//!
//! ## Deploy-time config
//!
//! Named for this provider and with dashes, not `vision:*`: an app's `[config]` keys must be
//! lowercase-with-dashes, and config is visible to every app on a host, so a second provider
//! must not share these.
//!
//! - `deepseek-model` — default `deepseek-chat`. (DeepSeek's reasoning models spend a small token
//!   budget thinking and return empty text, which this reports as `no-content`.)
//! - `deepseek-base-url` — default `https://api.deepseek.com`; point it at a compatible server to
//!   run without a key.
//! - the secret `deepseek-api-key`, required only when talking to `deepseek.com`.
//!
//! ## What this deliberately does not do
//!
//! It does not know what is in the picture. The prompt arrives from the caller, because the
//! caller is the one that has to parse the answer.

pub const DEFAULT_MODEL: &str = "deepseek-chat";
pub const DEFAULT_BASE: &str = "https://api.deepseek.com";

/// A ceiling on what is held in memory, not a policy: past this the read gives up rather than
/// growing until the store's memory cap traps the component and the connection just closes.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// What the vision model accepts. Checked here rather than passed through, because the far end's
/// error for an unsupported type is a 400 about a field, a bad place to learn a `.heic` was sent.
pub const MEDIA_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Why nothing came back, before it is mapped onto the contract's error type.
#[derive(Debug, PartialEq)]
pub enum Failure {
    InvalidRequest(String),
    ProviderDenied(String),
    ProviderUnavailable(String),
    BadResponse(String),
    NoContent,
}

/// Split a base URL into (https?, authority, path-prefix). Deliberately small: it only has to
/// handle what a deployment writes in config, a scheme, a host, a port and maybe a prefix.
pub fn split_base(base: &str) -> (bool, String, String) {
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

/// Whether a key is REQUIRED, which is a property of where this is pointed. The real API refuses
/// without one; a compatible local server ignores auth, and a tenant that has not been granted a
/// key is not asked for one.
pub fn key_required(authority: &str) -> bool {
    authority.ends_with("deepseek.com")
}

pub fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Base64, written out rather than pulled in: the only place this component needs it.
pub fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if c.len() > 1 { A[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// The caller's request, refused here when it cannot work, with the lowercased media type.
pub fn check_input(image: &[u8], media_type: &str) -> Result<String, Failure> {
    if image.is_empty() {
        return Err(Failure::InvalidRequest("the image is empty".into()));
    }
    if image.len() > MAX_BODY_BYTES {
        return Err(Failure::InvalidRequest("the image is too large".into()));
    }
    let media_type = media_type.to_ascii_lowercase();
    if !MEDIA_TYPES.contains(&media_type.as_str()) {
        return Err(Failure::InvalidRequest(format!(
            "unsupported media type {media_type} — one of {}",
            MEDIA_TYPES.join(", ")
        )));
    }
    Ok(media_type)
}

/// The Chat Completions body: the prompt, then the picture as a data URL. `temperature` 0 because
/// a caller that parses the answer wants the same picture to say the same thing.
pub fn request_body(model: &str, media_type: &str, image: &[u8], prompt: &str) -> String {
    // `data` is base64, so safe between quotes as it stands; everything else goes through the
    // JSON encoder.
    format!(
        "{{\"model\":{},\"temperature\":0,\"max_tokens\":1024,\"messages\":[{{\"role\":\"user\",\"content\":[\
         {{\"type\":\"text\",\"text\":{}}},\
         {{\"type\":\"image_url\",\"image_url\":{{\"url\":\"data:{};base64,{}\"}}}}]}}]}}",
        json_str(model),
        json_str(prompt),
        media_type,
        b64(image),
    )
}

/// What a reply means. The status decides which error, because what a caller should DO differs:
/// 401/402/403 is a deployment problem, 429 and 5xx are worth retrying, a 400 is this request.
pub fn interpret(status: u16, bytes: &[u8]) -> Result<String, Failure> {
    let parsed: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| Failure::BadResponse(format!("not json: {e}")))?;
    if status != 200 {
        let detail = parsed["error"]["message"].as_str().unwrap_or("no message").to_string();
        return Err(match status {
            400 | 422 => Failure::InvalidRequest(detail),
            401..=403 => Failure::ProviderDenied(detail),
            429 | 500..=599 => Failure::ProviderUnavailable(detail),
            _ => Failure::BadResponse(format!("{status}: {detail}")),
        });
    }
    // The message is a string; a compatible server may send a list of parts. Take every text
    // part, joined: taking only the first silently truncates a reply in the middle of its JSON.
    let content = &parsed["choices"][0]["message"]["content"];
    let text = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter(|p| p["type"] == "text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    };
    if text.trim().is_empty() {
        // It looked and said nothing: a safety filter, a reasoning model that spent its budget
        // thinking, or an empty reply. Not a transport failure, and not something a retry fixes.
        return Err(Failure::NoContent);
    }
    Ok(text)
}

/// The wasm side: the guest imports and the one export. Compiled only for the component, so
/// the pure core above and its tests build and run on the host, where those imports do not exist.
#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
mod guest {
    use super::*;
    use crate::bindings::comp::secrets::reader as secrets;
    use crate::bindings::exports::vision::describe::describer::{DescribeError, Guest};
    use crate::bindings::wasi::config::store as config;
    use crate::bindings::wasi::http::outgoing_handler;
    use crate::bindings::wasi::http::types::{
        Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
    };
    use crate::bindings::wasi::io::streams::StreamError;

    struct Component;

    impl From<Failure> for DescribeError {
        fn from(f: Failure) -> Self {
            match f {
                Failure::InvalidRequest(m) => DescribeError::InvalidRequest(m),
                Failure::ProviderDenied(m) => DescribeError::ProviderDenied(m),
                Failure::ProviderUnavailable(m) => DescribeError::ProviderUnavailable(m),
                Failure::BadResponse(m) => DescribeError::BadResponse(m),
                Failure::NoContent => DescribeError::NoContent,
            }
        }
    }

    pub fn config_or(key: &str, default: &str) -> String {
        config::get(key)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    fn api_key() -> Option<String> {
        match secrets::get("deepseek-api-key") {
            Ok(Some(s)) => secrets::reveal(&s).ok().filter(|v| !v.is_empty()),
            _ => None,
        }
    }

    /// POST a JSON body to `deepseek-base-url` + `/chat/completions`; (status, response bytes).
    fn post(body: &[u8]) -> Result<(u16, Vec<u8>), String> {
        let (tls, authority, prefix) = split_base(&config_or("deepseek-base-url", DEFAULT_BASE));
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let _ = headers.set("content-length", &[body.len().to_string().into_bytes()]);
        let _ = headers.set("connection", &[b"close".to_vec()]);
        if let Some(k) = api_key() {
            let _ = headers.set("authorization", &[format!("Bearer {k}").into_bytes()]);
        }
        let req = OutgoingRequest::new(headers);
        let e = |m: &str| m.to_string();
        req.set_method(&Method::Post).map_err(|_| e("method"))?;
        req.set_scheme(Some(&if tls { Scheme::Https } else { Scheme::Http }))
            .map_err(|_| e("scheme"))?;
        req.set_authority(Some(&authority)).map_err(|_| e("authority"))?;
        req.set_path_with_query(Some(&format!("{prefix}/chat/completions")))
            .map_err(|_| e("path"))?;
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
        // A vision call on a large photograph is slow, and a timeout shorter than the model is a
        // provider that "fails" on exactly the pictures worth sending.
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

    impl Guest for Component {
        fn describe(
            image: Vec<u8>,
            media_type: String,
            prompt: String,
        ) -> Result<String, DescribeError> {
            let media_type = check_input(&image, &media_type)?;
            // Refused HERE rather than by the far end: a request with no key comes back as a 401
            // about an Authorization header, which reads as a bug in this component rather than as
            // a deployment that never granted the secret. Only when a key is actually needed.
            let (_, authority, _) = split_base(&config_or("deepseek-base-url", DEFAULT_BASE));
            if key_required(&authority) && api_key().is_none() {
                return Err(DescribeError::ProviderDenied(format!(
                    "no deepseek-api-key in the vault, and {authority} requires one. Point \
                     `deepseek-base-url` at a compatible server to run without a key."
                )));
            }
            let body = request_body(
                &config_or("deepseek-model", DEFAULT_MODEL),
                &media_type,
                &image,
                &prompt,
            );
            let (status, bytes) =
                post(body.as_bytes()).map_err(DescribeError::ProviderUnavailable)?;
            interpret(status, &bytes).map_err(DescribeError::from)
        }
    }

    crate::bindings::export!(Component with_types_in crate::bindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_vectors_including_padding() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"M"), "TQ==");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"Man"), "TWFu");
        assert_eq!(b64(b"Many hands"), "TWFueSBoYW5kcw==");
        assert_eq!(b64(&[0xff, 0xd8, 0xff]), "/9j/");
    }

    #[test]
    fn a_base_url_splits_into_scheme_authority_and_prefix() {
        assert_eq!(
            split_base("https://api.deepseek.com"),
            (true, "api.deepseek.com".into(), "".into())
        );
        assert_eq!(
            split_base("http://127.0.0.1:8787/"),
            (false, "127.0.0.1:8787".into(), "".into())
        );
        assert_eq!(split_base("https://h.example/v1/"), (true, "h.example".into(), "/v1".into()));
        assert_eq!(split_base("h.example"), (true, "h.example".into(), "".into()));
    }

    #[test]
    fn a_key_is_required_only_for_the_real_api() {
        assert!(key_required("api.deepseek.com"));
        assert!(!key_required("127.0.0.1:8787"));
        assert!(!key_required("localhost:11434"));
    }

    #[test]
    fn the_input_is_checked_before_anything_is_sent() {
        assert_eq!(check_input(&[1], "IMAGE/JPEG").unwrap(), "image/jpeg");
        assert!(
            matches!(check_input(&[], "image/png"), Err(Failure::InvalidRequest(m)) if m.contains("empty"))
        );
        assert!(
            matches!(check_input(&[1], "image/heic"), Err(Failure::InvalidRequest(m)) if m.contains("unsupported"))
        );
        assert!(
            matches!(check_input(&vec![0; MAX_BODY_BYTES + 1], "image/png"), Err(Failure::InvalidRequest(m)) if m.contains("large"))
        );
    }

    #[test]
    fn the_request_is_valid_json_with_the_prompt_then_the_picture_as_a_data_url() {
        let body = request_body(
            "deepseek-chat",
            "image/png",
            &[0xff, 0xd8, 0xff],
            "what \"is\" this?\nbriefly",
        );
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(v["model"], "deepseek-chat");
        assert_eq!(v["temperature"], 0);
        let parts = &v["messages"][0]["content"];
        assert_eq!(
            parts[0],
            serde_json::json!({"type": "text", "text": "what \"is\" this?\nbriefly"})
        );
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,/9j/");
    }

    #[test]
    fn a_reply_is_its_text_and_an_empty_one_is_no_content() {
        let ok = br#"{"choices":[{"message":{"role":"assistant","content":"Red"}}]}"#;
        assert_eq!(interpret(200, ok).unwrap(), "Red");
        let parts = br#"{"choices":[{"message":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}]}"#;
        assert_eq!(interpret(200, parts).unwrap(), "ab", "every text part, joined");
        let empty = br#"{"choices":[{"message":{"content":"  "}}]}"#;
        assert_eq!(interpret(200, empty), Err(Failure::NoContent));
        assert_eq!(interpret(200, br#"{"choices":[]}"#), Err(Failure::NoContent));
        assert!(matches!(interpret(200, b"<html>"), Err(Failure::BadResponse(_))));
    }

    #[test]
    fn the_status_decides_what_a_caller_should_do() {
        let err = |s| interpret(s, br#"{"error":{"message":"nope"}}"#).unwrap_err();
        assert_eq!(err(400), Failure::InvalidRequest("nope".into()));
        assert_eq!(err(401), Failure::ProviderDenied("nope".into()));
        assert_eq!(
            err(402),
            Failure::ProviderDenied("nope".into()),
            "insufficient balance is a deployment problem"
        );
        assert_eq!(err(429), Failure::ProviderUnavailable("nope".into()));
        assert_eq!(err(503), Failure::ProviderUnavailable("nope".into()));
        assert!(matches!(err(418), Failure::BadResponse(m) if m.starts_with("418")));
    }
}
