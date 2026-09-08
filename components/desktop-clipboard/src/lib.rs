//! `desktop-clipboard` — read the desktop session's clipboard
//!
//! Reading the clipboard needs the desktop session's pasteboard, and a
//! `wasm32-wasip2` guest has none of those (ADR-0095). This is the component
//! side: it holds the contract, and reaches the daemon over HTTP the same
//! way `fs-watcher` reaches `comp-fswatch`.
//!
//! What this may dial is a MANIFEST decision (ADR-0008). The daemon's address
//! is `clipboard-url` in `wasi:config`, and the deployment's egress allow-list
//! decides whether the call leaves at all. A component that could reach any
//! address would make the allow-list decorative.
//!
//! Config (wasi:config/store):
//!   clipboard-url    where `comp-clipboard` is listening, e.g. http://127.0.0.1:8003
//!
//! It used to return `"UNIMPLEMENTED: ..."` — honest, but a `-> string`
//! contract cannot express failure any better than it can express success.
//! The current one can, including telling "nothing on the clipboard" apart
//! from "could not ask".

#[allow(warnings)]
mod bindings;

use bindings::exports::os::desktop::clipboard::{ClipboardError, Guest};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme};
use bindings::wasi::io::streams::StreamError;

struct Component;

/// Ten seconds. Reading the clipboard is a local desktop-API call on the
/// daemon's side; anything slower than this is a daemon in trouble, and a
/// caller waiting on an HTTP request would rather hear that than wait.
const TIMEOUT_NS: u64 = 10_000_000_000;

fn daemon_url() -> Result<String, ClipboardError> {
    match config::get("clipboard-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable rather than empty: nobody looked at the clipboard, the
        // deployment simply never said where the daemon is.
        _ => Err(ClipboardError::Unavailable(
            "clipboard-url is not set — this component has nowhere to ask".into(),
        )),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), ClipboardError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(ClipboardError::Unavailable(format!("clipboard-url must be http(s), got {url:?}")));
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
fn post(body: Vec<u8>) -> Result<Vec<u8>, ClipboardError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| ClipboardError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    // A shared secret, if the deployment set one — see
    // `comp_reconciler::daemon_auth`'s own doc for why loopback
    // binding alone is not a boundary. Absent means the daemon was
    // started with no --token, so there is nothing to send.
    if let Ok(Some(token)) = config::get("clipboard-token") {
        if !token.is_empty() {
            let _ = headers.set(&"authorization".to_string(), &[format!("Bearer {token}").into_bytes()]);
        }
    }
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}/call"))).map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
        // Chunked: `blocking-write-and-flush` traps above 4096 bytes.
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
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &json[at..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(&rest[start..end])
}

impl Guest for Component {
    fn read() -> Result<String, ClipboardError> {
        let raw = post(b"{}".to_vec())?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        // The daemon reports its refusals in the body. Mapping "empty" back
        // to its own case matters: a caller that cannot tell "nothing there"
        // from "ask again later" would retry a poll that will never change.
        if let Some(err) = field(&text, "error") {
            return Err(match err {
                "empty" => ClipboardError::Empty,
                other => {
                    let detail = field(&text, "detail").unwrap_or_default().to_string();
                    ClipboardError::Unavailable(format!("{other}: {detail}"))
                }
            });
        }

        Ok(field(&text, "text").unwrap_or_default().to_string())
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_read_back() {
        let json = r#"{"text":"hello clipboard"}"#;
        assert_eq!(field(json, "text"), Some("hello clipboard"));
        assert!(field(json, "error").is_none());
    }

    /// "empty" carries no detail field — a caller reading a missing field as
    /// "unavailable" would misreport a clipboard with nothing on it.
    #[test]
    fn an_empty_clipboard_is_told_apart_from_an_unavailable_one() {
        let empty = r#"{"error":"empty"}"#;
        assert_eq!(field(empty, "error"), Some("empty"));
        assert!(field(empty, "detail").is_none());

        let down = r#"{"error":"unavailable","detail":"no desktop session"}"#;
        assert_eq!(field(down, "error"), Some("unavailable"));
        assert_eq!(field(down, "detail"), Some("no desktop session"));
    }
}
