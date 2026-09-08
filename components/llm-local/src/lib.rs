//! `llm-local` — send a prompt to a language model running on this machine
//!
//! Running a model needs a runtime and weights on disk, and a `wasm32-wasip2`
//! guest has neither (ADR-0095). This is the component side: it holds the
//! contract and reaches `comp-llmlocal` over HTTP the same way `fs-watcher`
//! reaches `comp-fswatch`. The daemon proxies to a local Ollama server — see
//! its own doc comment for the honest boundary that implies.
//!
//! Config (wasi:config/store):
//!   llmlocal-url    where `comp-llmlocal` is listening, e.g. http://127.0.0.1:8006
//!
//! It used to return `format!("UNIMPLEMENTED: ...")` — a string shaped like an
//! answer, which no caller could tell from a real one. That is what the old
//! `-> string` contract permitted; the current one cannot express it.

#[allow(warnings)]
mod bindings;

use bindings::exports::ai::local::local::{Guest, InferError};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme};
use bindings::wasi::io::streams::StreamError;

struct Component;

/// A local model can take a while to answer, and a caller waiting on an HTTP
/// request would rather hear "unavailable" than hang indefinitely — but a
/// real generation is slower than a directory poll, so this is minutes rather
/// than fs-watcher's ten seconds.
const TIMEOUT_NS: u64 = 120_000_000_000;

fn daemon_url() -> Result<String, InferError> {
    match config::get("llmlocal-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable rather than any other variant: nobody refused anything,
        // the deployment simply never said where the model daemon is.
        _ => Err(InferError::Unavailable(
            "llmlocal-url is not set — this component has nowhere to ask".into(),
        )),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), InferError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(InferError::Unavailable(format!("llmlocal-url must be http(s), got {url:?}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), String::new()),
    };
    Ok((scheme, authority, path))
}

/// POST the request and return the body. Every failure is `unavailable`: the
/// daemon's own refusals arrive as JSON with a 200, so anything at this level
/// is the transport rather than an answer.
fn post(body: Vec<u8>) -> Result<Vec<u8>, InferError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| InferError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    // A shared secret, if the deployment set one — see
    // `comp_reconciler::daemon_auth`'s own doc for why loopback
    // binding alone is not a boundary. Absent means the daemon was
    // started with no --token, so there is nothing to send.
    if let Ok(Some(token)) = config::get("llmlocal-token") {
        if !token.is_empty() {
            let _ = headers.set(&"authorization".to_string(), &[format!("Bearer {token}").into_bytes()]);
        }
    }
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}/infer"))).map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
        // Chunked: `blocking-write-and-flush` traps above 4096 bytes, and a
        // prompt is small but not bounded.
        for chunk in body.chunks(4096) {
            stream.blocking_write_and_flush(chunk).map_err(|e| net(&format!("body write: {e:?}")))?;
        }
    }
    OutgoingBody::finish(out, None).map_err(|_| net("finish"))?;

    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_first_byte_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_between_bytes_timeout(Some(TIMEOUT_NS));

    let fut = outgoing_handler::handle(req, Some(opts)).map_err(|e| net(&format!("handle: {e:?}")))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| net("no response"))?
        .map_err(|_| net("response taken"))?
        .map_err(|e| net(&format!("http: {e:?}")))?;

    let body = resp.consume().map_err(|_| net("consume"))?;
    let stream = body.stream().map_err(|_| net("stream"))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(c) if c.is_empty() => break,
            Ok(c) => buf.extend_from_slice(&c),
            // `Closed` is end-of-body; anything else is a read that went
            // wrong, and returning what arrived would be a truncated answer
            // presented as a whole one.
            Err(StreamError::Closed) => break,
            Err(e) => return Err(net(&format!("read: {e:?}"))),
        }
    }
    Ok(buf)
}

/// Pull one JSON string field out without a parser.
///
/// The daemon's replies have two shapes and no nesting, so a full serde
/// dependency would be more code than the thing it reads.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &json[at..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(&rest[start..end])
}

impl Guest for Component {
    fn infer(prompt: String) -> Result<String, InferError> {
        let body = format!("{{\"prompt\":{}}}", json_str(&prompt));
        let raw = post(body.into_bytes())?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        if let Some(err) = field(&text, "error") {
            let detail = field(&text, "detail").unwrap_or_default().to_string();
            return Err(InferError::Unavailable(format!("{err}: {detail}")));
        }

        field(&text, "response")
            .map(str::to_string)
            .ok_or_else(|| InferError::Unavailable("daemon reply had no response field".into()))
    }
}

/// A JSON string literal. A prompt can contain a quote or a backslash, and
/// this is building a request out of one.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    /// A prompt is caller-supplied and goes into a JSON request. A quote in it
    /// would end the string early and the rest would be read as structure.
    #[test]
    fn a_prompt_cannot_break_out_of_the_request_it_is_put_in() {
        assert_eq!(json_str(r#"hello"#), r#""hello""#);
        assert_eq!(json_str(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(json_str("a\\b"), r#""a\\b""#);
        assert_eq!(json_str("a\nb"), r#""a\nb""#);
        // Escaped, not stripped: a control character silently removed would
        // change the prompt the daemon sees, and nobody would know.
        assert_eq!(json_str("a\u{1}b"), r#""a\u0001b""#);
    }

    #[test]
    fn a_response_field_is_read_back() {
        let json = r#"{"response":"the answer"}"#;
        assert_eq!(field(json, "response"), Some("the answer"));
        assert!(field(json, "error").is_none());
    }

    #[test]
    fn an_error_field_carries_its_detail() {
        let json = r#"{"error":"unavailable","detail":"connection refused"}"#;
        assert_eq!(field(json, "error"), Some("unavailable"));
        assert_eq!(field(json, "detail"), Some("connection refused"));
    }
}
